// Kareenos Desktop Agent — the activity overlay (a click-through pill the app
// shows while a command runs). It only mirrors Status.busy.
(function () {
  'use strict'
  var T = window.__TAURI__
  var what = document.getElementById('what')
  // Status.busy reads "Sara's Invoice Clerk · Typing into Name": the actor in ink, the action soft.
  function apply (s) {
    if (!s || !s.busy) return
    var i = s.busy.indexOf(' · ')
    what.textContent = ''
    if (i > 0) {
      var who = document.createElement('b'); who.textContent = s.busy.slice(0, i)
      what.appendChild(who); what.appendChild(document.createTextNode(' · ' + s.busy.slice(i + 3)))
    } else {
      what.textContent = s.busy
    }
  }
  T.event.listen('status', function (ev) { apply(ev.payload) })
  T.core.invoke('get_status').then(apply).catch(function () {})
})()
