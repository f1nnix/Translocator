# The standard screenshots of the GTK app, for `linux/dev.sh run`:
#
#   linux/dev.sh run linux/screenshots.sh
#
# Sourced by linux/run-headless.sh once the window is up, in a home with
# Documents (files), Downloads (40 logs) and Projects. Uses its helpers:
# key, text, pause, shot. Cmd in the shared keys is Ctrl here.

shot 01-start

# Left pane into Documents, right pane into Downloads.
key Down Down Return
pause 1
key Tab
key Down Down Down Return
pause 1
key Tab
pause 0.5
shot 02-two-folders

# Marks: two files in Documents.
key Down Down space space
pause 0.5
shot 03-marks

# The type-ahead filter.
text pho
pause 0.8
shot 04-filter
key Escape
pause 0.5

# New Folder (F7): the dialog, then a name typed into it.
key F7
pause 1.2
shot 05-new-folder
text Reports
pause 0.5
shot 06-new-folder-typed
key Return
pause 1.2
shot 07-created

# The console, after a command.
key ctrl+e
text "ls -la"
key Return
pause 1.5
shot 08-console

# Back to the list; the primary menu (F10).
key Escape
key F10
pause 0.8
shot 09-primary-menu
key Escape
pause 0.5

# The locations menu from the keyboard (ctrl-k).
key ctrl+k
pause 0.8
shot 10-locations
key Escape
pause 0.5

# The context menu of a row.
click 200 285 3
pause 0.8
shot 11-context-menu
key Escape
pause 0.5

# Drag todo.txt from the left pane onto the right one (a copy).
drag 60 395 1000 450
pause 1.2
shot 12-drag-drop
key Return
pause 1.5
shot 13-copied

# Dark.
click 1218 28
pause 1
shot 14-dark

# Back to light; this folder saved on the row of places, a second tab,
# hidden files shown.
click 1218 28
pause 0.5
key ctrl+d
key ctrl+t
key ctrl+shift+period
pause 1
shot 15-places-tabs-hidden

# A filter nothing matches.
text zzz
pause 0.8
shot 16-no-match
key Escape

# Quick look, in place of the other pane.
key ctrl+y
pause 1
shot 17-quick-look
key ctrl+y
pause 0.5

# A narrow window drops columns.
xdotool windowsize "$(win)" 980 700
pause 1.5
shot 18-narrow
