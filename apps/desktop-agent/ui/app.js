// Kareenos Desktop Agent — the main window (welcome / sign-in / connected).
// Plain JS on the global Tauri API (withGlobalTauri); everything that matters
// happens in Rust, this file only renders a Status and forwards clicks.
(function () {
  'use strict'
  var T = window.__TAURI__
  var invoke = T.core.invoke
  var listen = T.event.listen
  var mode = ''
  var status = null

  function $ (id) { return document.getElementById(id) }
  function show (view) {
    ;['welcome', 'signin', 'connected'].forEach(function (v) { $('view-' + v).classList.toggle('hidden', v !== view) })
  }
  function msg (text, kind) {
    var el = $('msg')
    if (!text) { el.classList.add('hidden'); el.textContent = ''; return }
    el.textContent = text
    el.className = 'msg ' + (kind || '')
  }
  function chipFor (s) {
    switch (s.state) {
      case 'connected': return s.paused ? ['warn', 'Paused'] : ['ok', 'Connected']
      case 'connecting': return ['muted', 'Connecting…']
      case 'offline': return ['warn', 'Offline — reconnecting']
      case 'superseded': return ['warn', 'Another computer is connected']
      case 'platform_off': return ['warn', 'Off on the platform']
      case 'error': return ['bad', 'Error']
      default: return ['muted', 'Signed out']
    }
  }
  function hintFor (s) {
    switch (s.state) {
      case 'connected': return s.paused ? 'Agents are paused on this computer until you resume them.' : 'Agents may work in your programs now. They pause while you use the mouse or keyboard.'
      case 'connecting': return 'Reaching the Kareenos platform…'
      case 'offline': return 'The platform cannot be reached. The agent reconnects by itself; check the network if this stays.'
      case 'superseded': return 'Another computer signed in to this project, so this one stepped aside. Click “Use this computer” to take the project back.'
      case 'platform_off': return 'The Desktop Channel is switched off on this Kareenos platform. The agent waits and connects when it is turned on.'
      case 'error': return s.last_error || ''
      default: return ''
    }
  }
  function render () {
    var s = status
    if (!s) return
    $('version').textContent = 'v' + s.version + (s.hostname ? ' · ' + s.hostname : '')
    var c = chipFor(s)
    $('chip').className = 'chip ' + c[0]
    $('chip').textContent = c[1]
    $('f-host').textContent = s.app_url.replace(/^https?:\/\//, '')
    if (s.signed_in) {
      show('connected')
      $('c-title').textContent = c[1]
      $('c-project').textContent = s.project || '—'
      $('c-account').textContent = s.account || '—'
      $('c-user').textContent = s.user || '—'
      $('c-state').textContent = c[1] + (s.last_error && s.state !== 'error' ? ' · ' + s.last_error : '')
      $('c-busy').textContent = s.busy || 'idle'
      $('c-hint').textContent = hintFor(s)
      $('c-pause').textContent = s.paused ? 'Resume agents' : 'Pause agents'
      $('c-takeover').classList.toggle('hidden', s.state !== 'superseded')
    } else if (mode === 'welcome') {
      show('welcome')
    } else {
      show('signin')
      $('s-note').textContent = s.last_error || ''
    }
  }
  function refresh () {
    return invoke('get_status').then(function (s) { status = s; render() })
  }
  function connectWith (text) {
    var code = String(text || '').trim()
    if (!code) { msg('Paste the connection code first.', 'bad'); return }
    msg('Connecting…')
    invoke('sign_in', { input: code }).then(function (s) {
      status = s
      mode = 'connected'
      msg('Signed in. The tray icon turns to Connected in a moment.', 'ok')
      render()
    }).catch(function (e) { msg(String(e), 'bad') })
  }
  function pasteInto (id) {
    invoke('read_clipboard').then(function (t) { $(id).value = t || ''; if (!t) msg('The clipboard is empty.', 'bad') }).catch(function (e) { msg(String(e), 'bad') })
  }
  function open (which) { invoke('open_link', { which: which }).catch(function (e) { msg(String(e), 'bad') }) }

  $('w-open').addEventListener('click', function () { open('connect') })
  $('s-open').addEventListener('click', function () { open('connect') })
  $('w-paste').addEventListener('click', function () { pasteInto('w-code') })
  $('s-paste').addEventListener('click', function () { pasteInto('s-code') })
  $('w-connect').addEventListener('click', function () { connectWith($('w-code').value) })
  $('s-connect').addEventListener('click', function () { connectWith($('s-code').value) })
  $('c-pause').addEventListener('click', function () { invoke('set_paused', { on: !(status && status.paused) }).then(function (s) { status = s; render() }) })
  $('c-takeover').addEventListener('click', function () { invoke('take_over').then(function (s) { status = s; render() }) })
  $('c-signout').addEventListener('click', function () {
    invoke('sign_out').then(function (s) { status = s; mode = 'signin'; msg('This computer was signed out. To remove it from the project too, use “Sign out this computer” in Kareenos → Browsers & Desktops.', 'ok'); render() })
  })
  $('c-diag').addEventListener('click', function () { invoke('open_diagnostics') })
  $('lnk-diag').addEventListener('click', function (e) { e.preventDefault(); invoke('open_diagnostics') })
  $('lnk-help').addEventListener('click', function (e) { e.preventDefault(); open('docs') })
  $('c-close').addEventListener('click', function () { invoke('close_window') })

  listen('status', function (ev) { status = ev.payload; render() })
  listen('mode', function (ev) { mode = ev.payload || mode; msg(''); render() })

  invoke('take_mode').then(function (m) { mode = m || '' }).catch(function () {}).then(refresh)
})()
