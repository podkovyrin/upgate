Output demo/upgate-demo.gif

Set Shell "bash"
Set Width 1760
Set Height 1200
Set Padding 0
Set Margin 0
Set BorderRadius 0
Set FontSize 22
Set LineHeight 1.15
Set TypingSpeed 45ms
Set Theme "OneDark"

Hide
Type `export PATH="$PWD/target/release:$PATH"`
Enter
Type `export XDG_CONFIG_HOME="$(mktemp -d)" XDG_STATE_HOME="$(mktemp -d)"`
Enter
Type `cp -RL "$HOME/.config/mise" "$XDG_CONFIG_HOME/mise"`
Enter
Type "unset NO_COLOR; export TERM=xterm-256color COLORTERM=truecolor"
Enter
Type `cd "$XDG_STATE_HOME"`
Enter
Type "clear"
Enter
Show

Type "upgate apply --dry-run"
Sleep 400ms
Enter
Wait+Screen /ruff/
Sleep 2000ms
Down@200ms 2
Sleep 600ms
Space
Sleep 700ms
Type "C"
Sleep 2200ms
Enter
Wait+Screen /Status/
Sleep 2500ms

Hide
