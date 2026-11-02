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
  // 0.2.2: a pasted code is previewed first; a link waits in Rust (pending) until confirmed.
  var pendingCode = null
  var preview = null

  function $ (id) { return document.getElementById(id) }
  function show (view) {
    ;['welcome', 'signin', 'confirm', 'connected'].forEach(function (v) { $('view-' + v).classList.toggle('hidden', v !== view) })
  }
  function msg (text, kind) {
    var el = $('msg')
    if (!text) { el.classList.add('hidden'); el.textContent = ''; return }
    el.textContent = text
    el.className = 'msg ' + (kind || '')
  }
  function chipFor (s) {
    switch (s.state) {
      case 'connected':
        if (s.locked) return ['warn', 'Locked']
        if (s.paused) return ['warn', 'Paused']
        if (s.busy) return ['work', 'Working']
        return ['ok', 'Connected']
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
      case 'connected':
        if (s.locked) return 'This computer is locked or at the Windows sign-in screen, so agents wait. Sign in and keep it unlocked; an unattended computer must stay signed in.'
        if (s.paused) return 'Agents are paused on this computer until you resume them.'
        return s.managed ? 'This computer is managed by your IT department. Agents of the people allowed on it may work here; they pause while someone uses the mouse or keyboard.' : 'Agents may work in your programs now. They pause while you use the mouse or keyboard.'
      case 'connecting': return 'Reaching the Kareenos platform…'
      case 'offline': return 'The platform cannot be reached. The agent reconnects by itself; check the network if this stays.'
      case 'superseded': return 'Another computer signed in to this project, so this one stepped aside. Click “Use this computer” to take the project back.'
      case 'platform_off': return 'The Desktop Channel is switched off on this Kareenos platform. The agent waits and connects when it is turned on.'
      case 'error': return s.last_error || ''
      default: return ''
    }
  }
  function renderPreview (p) {
    $('p-host').textContent = p.host || '—'
    $('p-account').textContent = p.account || '—'
    var isCode = p.kind === 'code'
    $('p-who-k').textContent = isCode ? 'Connection code' : 'Signed in as'
    $('p-who').textContent = isCode ? 'a one-time code from the Computers table' : (p.user || '—')
    var comp = p.computer_name ? p.computer_name + (p.computer_code ? ' (' + p.computer_code + ')' : '') : (p.computer_code || (isCode ? '—' : 'this computer, for project ' + (p.project || '—')))
    $('p-computer').textContent = comp
    $('p-mode').textContent = p.mode === 'unattended' ? 'Unattended (works without anyone at the keyboard)' : 'Attended (agents work while you are present)'
    $('p-key').textContent = p.signed_by || '—'
    show('confirm')
  }
  function render () {
    var s = status
    if (!s) return
    $('version').textContent = 'v' + s.version
    var c = chipFor(s)
    $('chip').className = 'chip ' + c[0]
    $('chip').textContent = c[1]
    $('f-host').textContent = s.app_url.replace(/^https?:\/\//, '')
    if (mode === 'confirm') {
      if (preview) { renderPreview(preview); return }
      invoke('pending_preview').then(function (p) {
        if (p) { preview = p; renderPreview(p) } else { mode = s.signed_in ? 'connected' : 'signin'; render() }
      }).catch(function () { mode = 'signin'; render() })
      return
    }
    if (s.signed_in) {
      show('connected')
      var name = s.computer_name || s.hostname || 'This computer'
      $('c-computer').textContent = name
      $('c-code').textContent = s.computer_code || ''
      $('c-code').classList.toggle('hidden', !s.computer_code)
      $('c-kick').textContent = s.computer_mode === 'unattended' ? 'Unattended computer' : 'This computer'
      $('c-sub').textContent = s.managed ? 'Managed by your IT department' : (s.computer_mode === 'unattended' ? 'Works without anyone at the keyboard' : 'Agents work here while you are present')
      var live = s.state === 'connected' && !s.paused && !s.locked
      $('c-dot').className = 'dot ' + (live ? (s.busy ? 'live' : '') : 'quiet')
      $('c-state').textContent = c[1].toUpperCase() + (s.last_error && s.state !== 'error' ? ' · ' + s.last_error : '')
      $('c-actor-line').classList.toggle('hidden', !s.actor)
      $('c-actor').textContent = s.actor ? s.actor + ' is working here' : ''
      $('c-project').textContent = s.project || '—'
      $('c-account').textContent = s.account || '—'
      $('c-user').textContent = s.user || '—'
      $('c-busy').textContent = s.busy || 'idle'
      $('c-hint').textContent = hintFor(s)
      $('c-pause-text').textContent = s.paused ? 'Resume agents' : 'Pause agents'
      $('c-signout').classList.toggle('hidden', !!s.managed)
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
  // A pasted code: check it (signature, hosts) and show where it points; the sign-in
  // happens only when the person clicks Connect on the confirmation view.
  function connectWith (text) {
    var code = String(text || '').trim()
    if (!code) { msg('Paste the connection code first.', 'bad'); return }
    msg('')
    invoke('preview_record', { input: code }).then(function (p) {
      pendingCode = code
      preview = p
      mode = 'confirm'
      renderPreview(p)
    }).catch(function (e) { msg(String(e), 'bad') })
  }
  function signedIn (s) {
    status = s
    pendingCode = null
    preview = null
    mode = 'connected'
    msg('Signed in. The tray icon turns to Connected in a moment.', 'ok')
    render()
  }
  function confirmConnect () {
    msg('Connecting…')
    var p = pendingCode ? invoke('sign_in', { input: pendingCode }) : invoke('confirm_pending')
    p.then(signedIn).catch(function (e) { msg(String(e), 'bad') })
  }
  function cancelConnect () {
    pendingCode = null
    preview = null
    invoke('cancel_pending').catch(function () {})
    mode = (status && status.signed_in) ? 'connected' : 'signin'
    msg('Not connected.', '')
    render()
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
  $('p-connect').addEventListener('click', confirmConnect)
  $('p-cancel').addEventListener('click', cancelConnect)
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
  listen('mode', function (ev) {
    mode = ev.payload || mode
    if (mode === 'confirm') { preview = null; pendingCode = null }
    msg('')
    invoke('take_notice').then(function (n) { if (n) msg(n, 'bad') }).catch(function () {})
    render()
  })

  invoke('take_mode').then(function (m) { mode = m || '' }).catch(function () {}).then(refresh).then(function () {
    invoke('take_notice').then(function (n) { if (n) msg(n, 'bad') }).catch(function () {})
  })
})()
