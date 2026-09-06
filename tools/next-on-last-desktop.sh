#!/bin/bash
# On press (BTN_TL-BTN_TR-BTN_SELECT = L1+R1+View): the next normal window
# that opens will be placed on the last virtual desktop.
#
# Invokes the "deckery-next-on-last-desktop" shortcut that is registered by
# the deckery-windowtracker KWin script (loaded via the package mechanism,
# which is required for workspace.windowAdded signals to fire).
# The windowtracker sets a one-shot flag that clears itself after placing
# exactly one normalWindow on the last desktop.

qdbus org.kde.kglobalaccel /component/kwin \
    org.kde.kglobalaccel.Component.invokeShortcut \
    'deckery-next-on-last-desktop'
