#!/bin/sh
# -proto 指定なし(サーバ reflection 利用)で Commit 1 件。proto パース分のオーバーヘッド比較用
exec grpcurl -plaintext -d @ 127.0.0.1:8098 google.firestore.v1.Firestore/Commit < "/private/tmp/claude-501/-Users-h-nomura-projects-git-hidane/3e3b0b4b-079e-4dde-83d9-811b19117084/scratchpad/t7/scripts/grpc-commit.json" > /dev/null
