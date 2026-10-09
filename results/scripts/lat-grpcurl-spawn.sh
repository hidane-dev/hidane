#!/bin/sh
# grpcurl のプロセス起動のみ(ネットワーク無し)
exec grpcurl -version > /dev/null 2>&1
