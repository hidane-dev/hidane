#!/bin/sh
exec curl -s -o /dev/null "http://127.0.0.1:8093/v1/projects/demo-hidane/databases/(default)/documents/bench/doc1"
