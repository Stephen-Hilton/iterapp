#!/bin/sh
# greet — tiny demo feature for iterloop (see ../bizreq.md, ../techreq.md)

name="${1:-World}"
printf 'Hello, %s!\n' "$name"
