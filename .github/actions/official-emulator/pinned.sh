# shellcheck shell=bash disable=SC2034 # sourced; the variables are read by whoever sources it
# The official Cloud Firestore emulator that hidane's fixtures were recorded from: its version
# and its jar's SHA-256, as firebase-tools pins them in src/emulator/downloadableEmulatorInfo.json
# ("firestore": "version", "expectedChecksumSHA256"). The action starts this one unless it is
# given another, and the Official emulator workflow compares it with firebase-tools' latest
# release. Both read it with `.`.
PINNED_VERSION=1.22.0
PINNED_SHA256=9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c
