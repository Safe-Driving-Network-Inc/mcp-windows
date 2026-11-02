// Kareenos Desktop Agent — the Diagnostics window.
(function () {
  'use strict'
  var T = window.__TAURI__
  var invoke = T.core.invoke
  var listen = T.event.listen
  var status = null
  var trail = []

  function $ (id) { return document.getElementById(id) }
  function msg (text, kind) {
    var el = $('msg')
    if (!text) { el.classList.add('hidden'); return }
    el.textContent = text
    el.className = 'msg ' + (kind || '')
  }
  function fmt (ms) { try { return new Date(ms).toLocaleTimeString() } catch (e) { return String(ms) } }
  var ROWS = [
    ['State', function (s) { return s.state + (s.paused ? ' (paused)' : '') + (s.locked ? ' (locked)' : '') }],
    ['Computer', function (s) { return (s.computer_name || '—') + (s.computer_code ? ' (' + s.computer_code + ')' : '') + (s.computer_mode ? ' · ' + s.computer_mode : '') }],
    ['Managed', function (s) { return s.managed ? 'yes (service / policy)' : 'no' }],
    ['Working now', function (s) { return s.actor || '—' }],
    ['Contract', function (s) { return s.contract || '—' }],
    ['Project', function (s) { return s.project || '—' }],
    ['Account', function (s) { return s.account || '—' }],
    ['User', function (s) { return s.user || '—' }],
    ['Server', function (s) { return s.server }],
    ['Kareenos', function (s) { return s.app_url }],
    ['Token expires', function (s) { return s.token_expires || '—' }],
    ['Renewable', function (s) { return s.renewable === false ? 'no (sign in again before it expires)' : 'yes' }],
    ['Commands done', function (s) { return String(s.commands_done) + (s.last_result_code ? ' · last: ' + s.last_result_code : '') }],
    ['Right now', function (s) { return s.busy || 'idle' }],
    ['Last error', function (s) { return s.last_error || '—' }],
    ['Version', function (s) { return s.version }],
    ['Host name', function (s) { return s.hostname }],
    ['Install id', function (s) { return s.install_id }],
    ['Data folder', function (s) { return s.data_dir }],
    ['Logs folder', function (s) { return s.logs_dir }],
    ['Local policy', function (s) { return s.policy_path }]
  ]
  function render () {
    if (!status) return
    $('version').textContent = 'v' + status.version + ' · ' + status.hostname
    $('chip').textContent = status.locked ? 'Locked' : (status.busy ? 'Working' : status.state)
    $('chip').className = 'chip ' + (status.locked ? 'warn' : (status.busy ? 'work' : (status.state === 'connected' ? 'ok' : (status.state === 'error' ? 'bad' : 'warn'))))
    var kv = $('kv')
    kv.textContent = ''
    ROWS.forEach(function (r) {
      var k = document.createElement('div'); k.className = 'k'; k.textContent = r[0]
      var v = document.createElement('div'); v.className = 'v'; v.textContent = r[1](status)
      kv.appendChild(k); kv.appendChild(v)
    })
    var box = $('trail')
    box.textContent = ''
    trail.forEach(function (e) {
      var line = document.createElement('div')
      line.className = e.level
      var t = document.createElement('span'); t.className = 't'; t.textContent = fmt(e.at) + ' '
      line.appendChild(t)
      line.appendChild(document.createTextNode(e.level + ' ' + e.text))
      box.appendChild(line)
    })
    box.scrollTop = box.scrollHeight
  }
  function refresh () {
    return Promise.all([invoke('get_status'), invoke('get_trail')]).then(function (r) { status = r[0]; trail = r[1] || []; render() })
  }
  function report () {
    var lines = ['Kareenos Desktop Agent diagnostics', '']
    ROWS.forEach(function (r) { lines.push(r[0] + ': ' + r[1](status)) })
    lines.push('', 'Recent activity:')
    trail.forEach(function (e) { lines.push(fmt(e.at) + ' ' + e.level + ' ' + e.text) })
    return lines.join('\n')
  }
  $('b-logs').addEventListener('click', function () { invoke('open_link', { which: 'logs' }).catch(function (e) { msg(String(e), 'bad') }) })
  $('b-data').addEventListener('click', function () { invoke('open_link', { which: 'data' }).catch(function (e) { msg(String(e), 'bad') }) })
  $('b-copy').addEventListener('click', function () { invoke('copy_text', { text: report() }).then(function () { msg('Copied. Paste it into your support ticket.', 'ok') }).catch(function (e) { msg(String(e), 'bad') }) })
  $('b-updates').addEventListener('click', function () { msg('Checking for updates…'); invoke('check_updates') })
  $('b-close').addEventListener('click', function () { invoke('close_window') })
  listen('status', function (ev) { status = ev.payload; invoke('get_trail').then(function (t) { trail = t || []; render() }) })
  listen('update', function (ev) { msg(String(ev.payload || ''), 'ok') })
  refresh()
  setInterval(function () { invoke('get_trail').then(function (t) { if ((t || []).length !== trail.length) { trail = t || []; render() } }) }, 3000)
})()
