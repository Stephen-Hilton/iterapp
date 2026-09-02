//! Question-widget schema (decided 2026-09-01): a typed fields array.
//! Answers overwrite `value` in place; iter_data validates at write time.

use serde::{Deserialize, Serialize};

pub const FIELD_TYPES: &[&str] = &["text", "int", "checkbox", "radio", "combo"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetOption {
    pub value: String,
    #[serde(default)]
    pub desc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetField {
    pub key: String,
    #[serde(default)]
    pub label: String,
    #[serde(rename = "type")]
    pub ftype: String,
    #[serde(default)]
    pub options: Vec<WidgetOption>,
    #[serde(default)]
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionWidget {
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub detail: String,
    pub fields: Vec<WidgetField>,
}

/// Validate a widget json document. Returns a list of problems; empty = valid.
pub fn validate(value: &serde_json::Value) -> Vec<String> {
    let mut errs = Vec::new();
    let widget: QuestionWidget = match serde_json::from_value(value.clone()) {
        Ok(w) => w,
        Err(e) => return vec![format!("widget does not parse: {e}")],
    };
    if widget.title.trim().is_empty() {
        errs.push("title is required".into());
    }
    if widget.fields.is_empty() {
        errs.push("at least one field is required".into());
    }
    let mut seen = std::collections::HashSet::new();
    for f in &widget.fields {
        if f.key.trim().is_empty() {
            errs.push("field key must be non-empty".into());
        }
        if !seen.insert(f.key.clone()) {
            errs.push(format!("duplicate field key '{}'", f.key));
        }
        if !FIELD_TYPES.contains(&f.ftype.as_str()) {
            errs.push(format!("field '{}': unknown type '{}' (expected one of {:?})", f.key, f.ftype, FIELD_TYPES));
        }
        match f.ftype.as_str() {
            "checkbox" | "radio" | "combo" => {
                if f.options.is_empty() {
                    errs.push(format!("field '{}': type '{}' requires options", f.key, f.ftype));
                }
                if f.ftype == "checkbox" && !f.value.is_null() && !f.value.is_array() {
                    errs.push(format!("field '{}': checkbox value must be an array (multi-select)", f.key));
                }
                if (f.ftype == "radio" || f.ftype == "combo")
                    && !f.value.is_null()
                    && !f.value.is_string()
                {
                    errs.push(format!("field '{}': {} value must be a single string", f.key, f.ftype));
                }
            }
            "int" => {
                if !f.value.is_null() && !f.value.is_i64() && !f.value.is_u64() {
                    errs.push(format!("field '{}': int value must be an integer", f.key));
                }
            }
            _ => {}
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_widget_passes() {
        let w = json!({
            "title": "Which?",
            "summary": "s",
            "fields": [
                {"key": "choice", "type": "checkbox",
                 "options": [{"value": "A"}, {"value": "B"}], "value": []},
                {"key": "other", "type": "text", "value": ""},
                {"key": "age", "type": "int", "value": 42}
            ]
        });
        assert!(validate(&w).is_empty());
    }

    #[test]
    fn bad_widgets_bounce() {
        let w = json!({"title": "", "fields": []});
        let errs = validate(&w);
        assert!(errs.iter().any(|e| e.contains("title")));
        assert!(errs.iter().any(|e| e.contains("at least one field")));

        let w = json!({"title": "t", "fields": [
            {"key": "c", "type": "radio", "options": [{"value":"A"}], "value": ["A"]}
        ]});
        assert!(validate(&w).iter().any(|e| e.contains("single string")));

        let w = json!({"title": "t", "fields": [
            {"key": "c", "type": "nope", "value": null}
        ]});
        assert!(validate(&w).iter().any(|e| e.contains("unknown type")));
    }
}
