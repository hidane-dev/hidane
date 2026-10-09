#!/bin/sh
# grpcurl 自体のプロセス起動+接続+reflection 1 往復の基準(Firestore 操作なし)
exec grpcurl -plaintext 127.0.0.1:8093 list > /dev/null
