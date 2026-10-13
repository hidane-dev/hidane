# The README's demo: a shell without Java, where firebase-tools cannot start the official emulator
# and `hidane exec` runs the 200 firebase-admin tests all the same.
#
# The Demo workflow (.github/workflows/demo.yml) records it on a GitHub-hosted runner and uploads
# demo.gif, which goes to docs/assets/demo.gif. To record it elsewhere (Linux, bash 5): run
# `npm ci` here, put hidane and node_modules/.bin on PATH and Java 21 as $JAVA_HOME/bin, with no
# other Java on PATH, run the last command once with Java, then `vhs demo.tape`. The tape takes
# $JAVA_HOME/bin off PATH before it starts.

Output demo.gif

Set Shell "bash"
Set FontFamily "JetBrains Mono"
Set FontSize 20
Set Width 1400
Set Height 320
Set Padding 32
Set TypingSpeed 35ms
Set WaitTimeout 180s
Set WaitPattern /^\$$/
Set Theme { "name": "hidane", "background": "#141210", "foreground": "#EDE6DC", "cursor": "#E2553D", "selection": "#322D28", "black": "#141210", "red": "#E2553D", "green": "#8FB573", "yellow": "#D9A55B", "blue": "#7FA3C6", "magenta": "#C08AB5", "cyan": "#7DB5AE", "white": "#EDE6DC", "brightBlack": "#8C8479", "brightRed": "#F07A63", "brightGreen": "#A9CC8E", "brightYellow": "#E8BE7A", "brightBlue": "#9CBBDA", "brightMagenta": "#D3A5CA", "brightCyan": "#9CCDC6", "brightWhite": "#FBF8F2" }

Require firebase
Require hidane

Hide
Type `export PS1='$ ' PATH="$(printf %s "$PATH" | tr : '\n' | grep -vxF "$JAVA_HOME/bin" | paste -sd: -)" && unset JAVA_HOME && clear`
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
