//! DynamoDB backend: one physical table per logical table, named
//! `<prefix><logical>` (default prefix "iter3_"), generic pk(S)/sk(S) keys,
//! body as a JSON string attribute, PAY_PER_REQUEST billing.
//! Tables are auto-created if missing — creation is strictly additive and
//! never touches tables outside the prefix (e.g. pdy4-*).

use crate::storage::{Storage, StorageError};
use async_trait::async_trait;
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType,
    ScalarAttributeType,
};
use iter_core::{VersionRow, now_utc};
use serde_json::Value;
use std::collections::HashMap;

pub struct DdbBackend {
    client: Client,
    prefix: String,
}

fn berr<E: std::fmt::Debug>(e: E) -> StorageError {
    StorageError::Backend(format!("{e:?}"))
}

fn av_s(s: &str) -> AttributeValue {
    AttributeValue::S(s.to_string())
}

impl DdbBackend {
    pub async fn new(region: &str, prefix: &str) -> Result<Self, StorageError> {
        let cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.to_string()))
            .load()
            .await;
        let client = Client::new(&cfg);
        let backend = Self { client, prefix: prefix.to_string() };
        backend.ensure_tables().await?;
        Ok(backend)
    }

    fn phys(&self, logical: &str) -> String {
        format!("{}{}", self.prefix, logical)
    }

    async fn ensure_tables(&self) -> Result<(), StorageError> {
        let existing = self
            .client
            .list_tables()
            .send()
            .await
            .map_err(berr)?
            .table_names
            .unwrap_or_default();
        let mut created = Vec::new();
        for logical in iter_core::TABLES {
            let name = self.phys(logical);
            if existing.contains(&name) {
                continue;
            }
            self.client
                .create_table()
                .table_name(&name)
                .billing_mode(BillingMode::PayPerRequest)
                .attribute_definitions(
                    AttributeDefinition::builder()
                        .attribute_name("pk")
                        .attribute_type(ScalarAttributeType::S)
                        .build()
                        .map_err(berr)?,
                )
                .attribute_definitions(
                    AttributeDefinition::builder()
                        .attribute_name("sk")
                        .attribute_type(ScalarAttributeType::S)
                        .build()
                        .map_err(berr)?,
                )
                .key_schema(
                    KeySchemaElement::builder()
                        .attribute_name("pk")
                        .key_type(KeyType::Hash)
                        .build()
                        .map_err(berr)?,
                )
                .key_schema(
                    KeySchemaElement::builder()
                        .attribute_name("sk")
                        .key_type(KeyType::Range)
                        .build()
                        .map_err(berr)?,
                )
                .send()
                .await
                .map_err(berr)?;
            created.push(name);
        }
        // wait for any freshly created table to become ACTIVE
        for name in created {
            for _ in 0..60 {
                let desc = self.client.describe_table().table_name(&name).send().await;
                if let Ok(d) = desc {
                    if d.table
                        .and_then(|t| t.table_status)
                        .map(|s| s.as_str() == "ACTIVE")
                        .unwrap_or(false)
                    {
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        Ok(())
    }

    fn item_body(item: &HashMap<String, AttributeValue>) -> Option<Value> {
        item.get("body")
            .and_then(|v| v.as_s().ok())
            .and_then(|s| serde_json::from_str(s).ok())
    }
}

#[async_trait]
impl Storage for DdbBackend {
    async fn get(&self, table: &str, pk: &str, sk: &str) -> Result<Option<Value>, StorageError> {
        let out = self
            .client
            .get_item()
            .table_name(self.phys(table))
            .key("pk", av_s(pk))
            .key("sk", av_s(sk))
            .send()
            .await
            .map_err(berr)?;
        Ok(out.item.as_ref().and_then(Self::item_body))
    }

    async fn put(&self, table: &str, pk: &str, sk: &str, body: &Value) -> Result<(), StorageError> {
        // keep the native version attribute in step with the body: versioned
        // writes condition on it, so a plain put must not leave it missing
        let mut req = self
            .client
            .put_item()
            .table_name(self.phys(table))
            .item("pk", av_s(pk))
            .item("sk", av_s(sk))
            .item("body", av_s(&body.to_string()));
        if let Some(v) = body.get("version").and_then(|v| v.as_u64()) {
            req = req.item("version", AttributeValue::N(v.to_string()));
        }
        req.send().await.map_err(berr)?;
        Ok(())
    }

    async fn delete(&self, table: &str, pk: &str, sk: &str) -> Result<bool, StorageError> {
        let out = self
            .client
            .delete_item()
            .table_name(self.phys(table))
            .key("pk", av_s(pk))
            .key("sk", av_s(sk))
            .return_values(aws_sdk_dynamodb::types::ReturnValue::AllOld)
            .send()
            .await
            .map_err(berr)?;
        Ok(out.attributes.is_some())
    }

    async fn query(&self, table: &str, pk: &str) -> Result<Vec<Value>, StorageError> {
        let mut out = Vec::new();
        let mut last_key = None;
        loop {
            let mut req = self
                .client
                .query()
                .table_name(self.phys(table))
                .key_condition_expression("pk = :pk")
                .expression_attribute_values(":pk", av_s(pk));
            if let Some(k) = last_key {
                req = req.set_exclusive_start_key(Some(k));
            }
            let resp = req.send().await.map_err(berr)?;
            for item in resp.items() {
                if let Some(b) = Self::item_body(item) {
                    out.push(b);
                }
            }
            match resp.last_evaluated_key {
                Some(k) if !k.is_empty() => last_key = Some(k),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn scan(&self, table: &str) -> Result<Vec<Value>, StorageError> {
        let mut out = Vec::new();
        let mut last_key = None;
        loop {
            let mut req = self.client.scan().table_name(self.phys(table));
            if let Some(k) = last_key {
                req = req.set_exclusive_start_key(Some(k));
            }
            let resp = req.send().await.map_err(berr)?;
            for item in resp.items() {
                if let Some(b) = Self::item_body(item) {
                    out.push(b);
                }
            }
            match resp.last_evaluated_key {
                Some(k) if !k.is_empty() => last_key = Some(k),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn put_versioned(
        &self,
        table: &str,
        pk: &str,
        sk: &str,
        body: &Value,
        expect: u64,
    ) -> Result<(), StorageError> {
        let new_version = expect + 1;
        let mut req = self
            .client
            .put_item()
            .table_name(self.phys(table))
            .item("pk", av_s(pk))
            .item("sk", av_s(sk))
            .item("version", AttributeValue::N(new_version.to_string()))
            .item("body", av_s(&body.to_string()));
        if expect == 0 {
            req = req.condition_expression("attribute_not_exists(pk)");
        } else {
            req = req
                .condition_expression("version = :expect")
                .expression_attribute_values(":expect", AttributeValue::N(expect.to_string()));
        }
        match req.send().await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = format!("{e:?}");
                if msg.contains("ConditionalCheckFailed") {
                    let current = self.get(table, pk, sk).await.unwrap_or(None);
                    Err(StorageError::Conflict(current))
                } else {
                    Err(StorageError::Backend(msg))
                }
            }
        }
    }

    async fn acquire_lock(
        &self,
        table: &str,
        pk: &str,
        sk: &str,
        body: &Value,
        now: &str,
        holder_workid: &str,
    ) -> Result<(), StorageError> {
        let expires = body.get("expires").and_then(|v| v.as_str()).unwrap_or("");
        let req = self
            .client
            .put_item()
            .table_name(self.phys(table))
            .item("pk", av_s(pk))
            .item("sk", av_s(sk))
            .item("expires", av_s(expires))
            .item("workid", av_s(holder_workid))
            .item("body", av_s(&body.to_string()))
            .condition_expression("attribute_not_exists(pk) OR expires < :now OR workid = :wid")
            .expression_attribute_values(":now", av_s(now))
            .expression_attribute_values(":wid", av_s(holder_workid));
        match req.send().await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = format!("{e:?}");
                if msg.contains("ConditionalCheckFailed") {
                    let current = self.get(table, pk, sk).await.unwrap_or(None);
                    Err(StorageError::Conflict(current))
                } else {
                    Err(StorageError::Backend(msg))
                }
            }
        }
    }

    async fn bump_seq(&self, project: &str, logical_table: &str) -> Result<u64, StorageError> {
        let out = self
            .client
            .update_item()
            .table_name(self.phys("versions"))
            .key("pk", av_s(project))
            .key("sk", av_s(logical_table))
            .update_expression("ADD seq_num :one SET updated = :ts")
            .expression_attribute_values(":one", AttributeValue::N("1".into()))
            .expression_attribute_values(":ts", av_s(&now_utc()))
            .return_values(aws_sdk_dynamodb::types::ReturnValue::AllNew)
            .send()
            .await
            .map_err(berr)?;
        let seq = out
            .attributes
            .as_ref()
            .and_then(|a| a.get("seq_num"))
            .and_then(|v| v.as_n().ok())
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(seq)
    }

    async fn get_versions(&self, project: &str) -> Result<Vec<VersionRow>, StorageError> {
        let resp = self
            .client
            .query()
            .table_name(self.phys("versions"))
            .key_condition_expression("pk = :pk")
            .expression_attribute_values(":pk", av_s(project))
            .send()
            .await
            .map_err(berr)?;
        let mut out = Vec::new();
        for item in resp.items() {
            out.push(VersionRow {
                projectname: item.get("pk").and_then(|v| v.as_s().ok()).cloned().unwrap_or_default(),
                table: item.get("sk").and_then(|v| v.as_s().ok()).cloned().unwrap_or_default(),
                seq: item
                    .get("seq_num")
                    .and_then(|v| v.as_n().ok())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0),
                updated: item.get("updated").and_then(|v| v.as_s().ok()).cloned().unwrap_or_default(),
            });
        }
        Ok(out)
    }

    fn backend_name(&self) -> &'static str {
        "dynamodb"
    }
}
