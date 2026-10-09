#!/bin/sh
# curl のプロセス起動のみ(ネットワーク無し)
exec curl -s --version > /dev/null
