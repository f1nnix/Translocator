# One screenshot in a colour scheme, for `linux/dev.sh run`:
#
#   TL_SETTINGS='{"appearance":{"mode":"dark","dark":"Nord"}}' \
#       linux/dev.sh run linux/screenshots-theme.sh
#
# Shows the left pane in Documents with marks and the console.
key Down Down Return
pause 1
key Down Down space space
key ctrl+shift+j
pause 1
shot theme
