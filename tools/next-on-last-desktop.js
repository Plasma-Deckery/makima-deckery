// next-on-last-desktop.js — one-shot KWin script
//
// Loaded dynamically by next-on-last-desktop.sh each time the user triggers
// the "next window on last desktop" action. Connects to windowAdded, waits
// for the first normalWindow (real app window, not panel/tooltip/dock), moves
// it to the last virtual desktop, switches there, then disconnects so it
// never fires again.
//
// The shell script unloads any previous instance before loading this one,
// so repeated presses always give a fresh one-shot.

workspace.windowAdded.connect(function handler(w) {
    if (!w.normalWindow) { return; }
    workspace.windowAdded.disconnect(handler);

    var desktops = workspace.desktops;
    if (desktops.length === 0) { return; }

    var lastDesktop = desktops[desktops.length - 1];
    w.desktops = [lastDesktop];
    workspace.currentDesktop = lastDesktop;
});
