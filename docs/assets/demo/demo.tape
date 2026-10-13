# The README's demo, in two parts. First, the cost every CI job pays: firebase-tools starts the
# emulator, runs nothing (`true`) and stops it, with the official emulator and then with hidane.
# Then a shell without Java, where `hidane exec` runs the 200 firebase-admin tests all the same.
#
# The Demo workflow (.github/workflows/demo.yml) records it on a GitHub-hosted runner and uploads
# demo.gif, which goes to docs/assets/demo.gif. To record it elsewhere: run `npm ci` here, put Java
# 21 first on PATH (as $JAVA_HOME/bin), with no other Java on PATH, then hidane and
# node_modules/.bin; run each command once so that the emulator's jar is downloaded, then
# `vhs demo.tape` on a quiet machine. The second part takes $JAVA_HOME/bin off PATH.

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

Type "# start the emulator, run nothing, stop it: what every CI job pays"
Sleep 400ms
Enter
Type `time firebase emulators:exec "true" > /dev/null 2>&1`
Sleep 400ms
Enter
Wait
Sleep 1s
Type `time hidane exec -- firebase emulators:exec "true" > /dev/null 2>&1`
Sleep 400ms
Enter
Wait
Sleep 3s

Hide
Type `export PATH="$(printf %s "$PATH" | tr : '\n' | grep -vxF "$JAVA_HOME/bin" | paste -sd: -)" && unset JAVA_HOME && clear`
Enter
Show

Type "# a machine without Java"
Sleep 400ms
Enter
Type "java -version"
Sleep 400ms
Enter
Wait
Sleep 1s
Type `firebase emulators:exec "npm test" 2>&1 | grep -m1 -i java`
Sleep 400ms
Enter
Wait
Sleep 1s
Type `hidane exec -- firebase emulators:exec "npm test" 2>&1 | grep -w pass`
Sleep 400ms
Enter
Wait
Sleep 5s
