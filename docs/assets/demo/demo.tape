# The README's demo: the same 200 firebase-admin tests under `firebase emulators:exec`, first with
# the official emulator, then with hidane standing in for Java and the jar.
#
# The Demo workflow (.github/workflows/demo.yml) records it on a GitHub-hosted runner and uploads
# demo.gif, which goes to docs/assets/demo.gif. To record it elsewhere: run `npm ci` here, put Java
# 21, hidane and node_modules/.bin on PATH, run each command once so that the emulator's jar is
# downloaded, then `vhs demo.tape` on a quiet machine. stderr goes through grep too, so
# firebase-tools' warnings about the machine stay out of the recording.

Output demo.gif

Require firebase
Require hidane
Require java

Set Shell "bash"
Set FontFamily "JetBrains Mono"
Set FontSize 20
Set Width 1400
Set Height 420
Set Padding 32
Set TypingSpeed 35ms
Set WaitTimeout 180s
Set WaitPattern /^\$$/
Set Theme { "name": "hidane", "background": "#141210", "foreground": "#EDE6DC", "cursor": "#E2553D", "selection": "#322D28", "black": "#141210", "red": "#E2553D", "green": "#8FB573", "yellow": "#D9A55B", "blue": "#7FA3C6", "magenta": "#C08AB5", "cyan": "#7DB5AE", "white": "#EDE6DC", "brightBlack": "#8C8479", "brightRed": "#F07A63", "brightGreen": "#A9CC8E", "brightYellow": "#E8BE7A", "brightBlue": "#9CBBDA", "brightMagenta": "#D3A5CA", "brightCyan": "#9CCDC6", "brightWhite": "#FBF8F2" }

Hide
Type "export PS1='$ ' TIMEFORMAT='took %1Rs' && clear"
Enter
Show

Type "# 200 Firestore tests, on the official emulator"
Sleep 400ms
Enter
Type `time firebase emulators:exec --only firestore "npm test" 2>&1 | grep -E " (pass|fail) [0-9]+$"`
Sleep 400ms
Enter
Wait
Sleep 1.5s

Type "# the same tests, with hidane in place of Java and the jar"
Sleep 400ms
Enter
Type `time hidane exec -- firebase emulators:exec --only firestore "npm test" 2>&1 | grep -E " (pass|fail) [0-9]+$"`
Sleep 400ms
Enter
Wait
Sleep 5s
