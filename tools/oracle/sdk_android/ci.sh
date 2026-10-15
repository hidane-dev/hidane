#!/bin/sh
# Runs each section of the Android SDK harness against a fresh hidane, in the Android emulator adb
# reaches, and compares each transcript with the committed results/sdk-android-<section>-hidane.json.
# From the repository root, once the harness's APKs are built (`gradle :app:assembleDebug
# :app:assembleDebugAndroidTest` in tools/oracle/sdk_android):
#
#   tools/oracle/sdk_android/ci.sh <hidane binary> <output directory>
#
# The harness reaches hidane on the host as 10.0.2.2:8080.
set -u

hidane=$1
out=$2
apks=tools/oracle/sdk_android/app/build/outputs/apk
files=/sdcard/Android/data/dev.hidane.sdkandroid/files
mkdir -p "$out"
adb install -r -t "$apks/debug/app-debug.apk"
adb install -r -t "$apks/androidTest/debug/app-debug-androidTest.apk"

status=0
for section in documents queries transactions listen; do
  "$hidane" --host 127.0.0.1 --port 8080 > "$out/hidane-$section.log" 2>&1 &
  pid=$!
  for _ in $(seq 30); do curl -fsS http://127.0.0.1:8080/ > /dev/null && break; sleep 1; done
  adb shell rm -f "$files/$section.json"
  adb shell am instrument -w -e section "$section" -e port 8080 \
    dev.hidane.sdkandroid.test/androidx.test.runner.AndroidJUnitRunner
  adb pull "$files/$section.json" "$out/sdk-android-$section-hidane.json"
  kill "$pid"
  if diff -u "results/sdk-android-$section-hidane.json" "$out/sdk-android-$section-hidane.json"; then
    echo "$section: as committed"
  else
    echo "::error title=Android SDK::the $section transcript differs from results/sdk-android-$section-hidane.json"
    status=1
  fi
done
exit "$status"
