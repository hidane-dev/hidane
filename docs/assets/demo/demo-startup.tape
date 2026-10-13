# The README's start-up demo: each emulator on its own. `ready` starts it, prints how long its port
# took to accept a connection, and stops it. The official emulator is the jar firebase-tools
# downloaded, run by Java 21.
#
# The Demo workflow (.github/workflows/demo.yml) records it on a GitHub-hosted runner and uploads
# demo-startup.gif, which goes to docs/assets/demo-startup.gif. To record it elsewhere (Linux,
# bash 5): run `npm ci` here, put Java 21, hidane and node_modules/.bin on PATH, run
# `firebase emulators:exec true` once so that the emulator's jar is downloaded, then
# `vhs demo-startup.tape` on a quiet machine.

Output demo-startup.gif

Set Shell "bash"
Set FontFamily "JetBrains Mono"
Set FontSize 20
Set Width 1400
Set Height 260
Set Padding 32
Set TypingSpeed 35ms
Set WaitTimeout 180s
Set WaitPattern /^\$$/
Set Theme { "name": "hidane", "background": "#141210", "foreground": "#EDE6DC", "cursor": "#E2553D", "selection": "#322D28", "black": "#141210", "red": "#E2553D", "green": "#8FB573", "yellow": "#D9A55B", "blue": "#7FA3C6", "magenta": "#C08AB5", "cyan": "#7DB5AE", "white": "#EDE6DC", "brightBlack": "#8C8479", "brightRed": "#F07A63", "brightGreen": "#A9CC8E", "brightYellow": "#E8BE7A", "brightBlue": "#9CBBDA", "brightMagenta": "#D3A5CA", "brightCyan": "#9CCDC6", "brightWhite": "#FBF8F2" }

Require hidane
Require java

Hide
Type `export PS1='$ ' JAR="$(ls ~/.cache/firebase/emulators/cloud-firestore-emulator-*.jar)" && clear`
Enter
Show

Type "# each emulator on its own, until its port accepts a connection"
Sleep 400ms
Enter
Type `./ready java -jar "$JAR" --host 127.0.0.1 --port 8080`
Sleep 400ms
Enter
Wait
Sleep 1s
Type "./ready hidane --host 127.0.0.1 --port 8080"
Sleep 400ms
Enter
Wait
Sleep 4s
