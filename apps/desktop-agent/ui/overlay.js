// Kareenos Desktop Agent — the activity overlay (a click-through pill the app
// shows while a command runs). It only mirrors Status.busy.
(function () {
  'use strict'
  var T = window.__TAURI__
  var what = document.getElementById('what')
  function apply (s) { if (s && s.busy) what.textContent = s.busy }
  T.event.listen('status', function (ev) { apply(ev.payload) })
  T.core.invoke('get_status').then(apply).catch(function () {})
})()
