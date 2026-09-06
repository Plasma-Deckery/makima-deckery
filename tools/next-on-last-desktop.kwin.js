// NOTE: This file is NOT loaded by next-on-last-desktop.sh anymore.
//
// KWin scripts loaded dynamically via org.kde.kwin.Scripting.loadScript
// by file path do NOT receive workspace.windowAdded signals — only scripts
// loaded via the KWin package mechanism (installed under
// ~/.local/share/kwin/scripts/) get signal callbacks.
//
// The placement logic lives in the deckery-windowtracker package script:
//   ~/.local/share/kwin/scripts/deckery-windowtracker/contents/code/main.js
//
// The shell script now calls:
//   qdbus org.kde.kglobalaccel /component/kwin ... invokeShortcut 'deckery-next-on-last-desktop'
// which triggers the registerShortcut handler inside the windowtracker,
// setting a one-shot flag that places the next normalWindow on the last desktop.
