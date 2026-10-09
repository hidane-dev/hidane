#!/bin/sh
exec curl -s -o /dev/null -X PATCH "http://127.0.0.1:8093/v1/projects/demo-hidane/databases/(default)/documents/bench/doc1" -H 'Content-Type: application/json' -d '{"fields":{"v":{"integerValue":"1"},"s":{"stringValue":"hello"}}}'
