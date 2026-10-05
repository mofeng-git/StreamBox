const credentials = () => sessionStorage.getItem('streambox-auth')
const encodeCredentials = (username, password) => btoa(String.fromCharCode(...new TextEncoder().encode(`${username}:${password}`)))
const api = async (path, options = {}) => {
  const headers = { 'content-type': 'application/json', ...options.headers }
  if (credentials()) headers.authorization = `Basic ${credentials()}`
  const response = await fetch(`/api${path}`, { ...options, headers, signal: options.signal || AbortSignal.timeout(20000) })
  const text = await response.text()
  let data; try { data = JSON.parse(text) } catch (_) { data = { error: text } }
  if (response.status === 401) { sessionStorage.removeItem('streambox-auth'); state.message = '登录已失效，请重新登录'; render(); throw new Error(state.message) }
  if (!response.ok) throw new Error(data.error || data.message || `请求失败（${response.status}）`)
  return data
}

const state = { page: 'overview', status: { runtime: {}, media: {}, outputs: {} }, system: {}, config: { video: {}, audio: {}, recording: {}, streaming: {}, channels: [] }, videos: [], audios: [], recordings: [], logs: [], channels: [], storage: [], dirty: false, editRevision: 0, configEpoch: 0, encodingTab: 'video', previewExpanded: false, previewUrl: '', previewError: '', previewTime: '', smart: null, editChannelId: null, message: '', recordingBusy: false, fileSearch: '', fileType: 'all', playbackEpoch: 0 }
const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]))
const selected = (a, b) => a === b ? 'selected' : ''
const checked = value => value ? 'checked' : ''
const bytes = value => value > 1048576 ? `${(value / 1048576).toFixed(1)} MB` : `${(value / 1024).toFixed(1)} KB`
const statusText = value => ({ idle: '待机', running: '运行中', device_lost: '信号丢失', error: '异常' }[value] || '加载中')

let refreshing = false
let previewLoading = false
async function load(forceRender = true) {
  if (!credentials() || new URLSearchParams(location.search).get('login') === '1') { if (forceRender) render(); return }
  if (refreshing) return
  refreshing = true
  const revision=state.editRevision, configEpoch=state.configEpoch
  try {
    const entries = [['status','/status'], ['system','/system'], ['config','/config'], ['videos','/devices/video'], ['audios','/devices/audio'], ['recordings','/recordings'], ['logs','/logs'], ['channels','/channels'], ['storage','/storage']]
    const results = await Promise.allSettled(entries.map(async ([key, path]) => ({ key, value: await api(path) })))
    for (const result of results) {
      if (result.status === 'fulfilled') { if (result.value.key !== 'config' || (configEpoch===state.configEpoch && (!state.dirty || (forceRender && revision===state.editRevision)))) state[result.value.key] = result.value.value }
      else if (credentials()) state.message = result.reason.message
    }
    if (!credentials()) return
    if ((forceRender && (!state.dirty || revision===state.editRevision)) || (!state.dirty && ['overview','storage','system'].includes(state.page) && !state.previewExpanded && !document.querySelector('dialog[open]'))) render()
    else updateResources()
    updateRecordingStatus()
    syncVideoCapabilityControls()
  } finally { refreshing = false }
}
async function loadPreview() {
  const target = document.querySelector('[data-preview]')
  if (!target || !credentials() || previewLoading) return
  previewLoading = true
  try {
    const response = await fetch(`/api/preview?ts=${Date.now()}`, { headers: { authorization: `Basic ${credentials()}` }, cache: 'no-store', signal: AbortSignal.timeout(15000) })
    if (!response.ok) throw new Error((await response.text()) || '输入源暂时不可用')
    const next = URL.createObjectURL(await response.blob())
    const check = new Image(); check.src = next
    try { await check.decode() } catch (error) { URL.revokeObjectURL(next); throw new Error('输入图像无法解码') }
    if (state.previewUrl) URL.revokeObjectURL(state.previewUrl)
    state.previewUrl = next; state.previewError = ''; state.previewTime = new Date().toLocaleTimeString('zh-CN')
  } catch (error) { state.previewError = error.name === 'TimeoutError' ? '抓帧超时，请检查输入信号' : error.message }
  finally {
    previewLoading = false
    document.querySelectorAll('[data-preview]').forEach(image => { if (state.previewUrl) image.src = state.previewUrl; image.hidden = !state.previewUrl })
    document.querySelectorAll('[data-preview-status]').forEach(element => { element.textContent = state.previewError ? '预览暂不可用' : `已更新 ${state.previewTime}` })
    document.querySelectorAll('[data-preview-error]').forEach(element => { element.textContent = state.previewError || '正在获取输入图像…'; element.hidden = !!state.previewUrl && !state.previewError })
  }
}
const sizeText = bytes => bytes == null ? '—' : bytes >= 1073741824 ? `${(bytes / 1073741824).toFixed(1)} GB` : `${(bytes / 1048576).toFixed(0)} MB`
const percent = (used, total) => total ? Math.round(used / total * 100) : null
function resourceCards(mini = false) {
  const sys = state.system
  const data = [['CPU 使用率',sys.cpu_usage_percent == null ? null : Math.round(sys.cpu_usage_percent), sys.cpu_frequency_mhz == null ? '频率未知' : `${sys.cpu_frequency_mhz} MHz`, 'blue','cpu'], ['内存使用率',percent(sys.memory_used_bytes,sys.memory_total_bytes), `${sizeText(sys.memory_used_bytes)} / ${sizeText(sys.memory_total_bytes)}`, 'cyan','memory'], ['CPU 温度',sys.temperature_celsius == null ? null : +sys.temperature_celsius.toFixed(1), sys.temperature_celsius == null ? '无温度传感器数据' : (sys.temperature_celsius < 80 ? '正常' : '温度偏高'), 'green','temperature'], ['磁盘使用率',percent(sys.disk_used_bytes,sys.disk_total_bytes), `${sizeText(sys.disk_used_bytes)} / ${sizeText(sys.disk_total_bytes)}`, 'blue','disk']]
  return `<div class="resource-grid ${mini ? 'mini' : ''}" data-resources>${data.map(([label,value,detail,color,key]) => `<div class="ring-card"><span>${label}</span><div class="ring ring-${color}" style="--progress:${value == null ? 0 : Math.min(100,value)}%;--ring-color:${color === 'green' ? '#08aa61' : '#087bff'}"><b>${value == null ? '—' : value + (key === 'temperature' ? '°C' : '%')}</b></div><small>${esc(detail)}</small></div>`).join('')}</div>`
}
function updateResources() { document.querySelectorAll('[data-resources]').forEach(element => { const holder = document.createElement('div'); holder.innerHTML = resourceCards(element.classList.contains('mini')); element.replaceWith(holder.firstElementChild) }) }
function previewBox() {
  return `<div class="preview-box ${state.previewExpanded ? 'preview-expanded' : ''}" ${state.previewExpanded ? 'role="dialog" aria-label="输入预览" aria-modal="true"' : ''}><img data-preview ${state.previewUrl ? `src="${state.previewUrl}"` : 'hidden'} alt="输入源当前图像"><div class="preview-placeholder" data-preview-error ${state.previewUrl && !state.previewError ? 'hidden' : ''}>${esc(state.previewError || '正在获取输入图像…')}</div><div class="preview-meta"><b data-recording-status class="preview-recording ${state.status.runtime.recording ? 'is-recording' : ''}"><i></i><span>${state.status.runtime.recording ? '录制中' : '未开始录制'}</span></b><button class="preview-expand" data-fullscreen aria-label="${state.previewExpanded ? '缩回预览' : '放大预览'}" aria-expanded="${state.previewExpanded}">${state.previewExpanded ? '↙ 缩回' : '⛶ 放大'}</button></div></div>`
}
function videoFormatText(format) { return ({ mjpeg: 'MJPEG', yuyv422: 'YUYV', h264: 'H.264', h265: 'H.265' })[format] || format?.toUpperCase() || '—' }
function videoModeKey(mode) { return `${mode.resolution}|${mode.frame_rate}|${mode.format}` }
function videoModeText(mode) { return `${mode.resolution.replace('x', ' × ')}@${mode.frame_rate}fps ${videoFormatText(mode.format)}` }
function audioModeKey(mode) { return `${mode.sample_rate}|${mode.format}` }
function configuredVideo() { return state.videos.find(device => device.path === (state.config.video.device || state.status.runtime.selected_video)) || (!state.config.video.device ? state.videos[0] : null) }


async function action(path) {
  try { await api(path, { method: 'POST' }); state.message = '操作成功'; await load() } catch (error) { state.message = error.message; render() }
}

const control = (id, value, type = 'text') => `<input id="${id}" type="${type}" value="${esc(value)}" class="control">`
const select = (id, value, options) => `<select id="${id}" class="control">${options.map(([key, label]) => `<option value="${key}" ${selected(value, key)}>${label}</option>`).join('')}</select>`
const field = (label, body) => `<label class="field"><span>${label}</span>${body}</label>`

function channelEditor() {
  const channels = state.config.channels || []
  const editing = channels.find(channel => channel.id === state.editChannelId)
  const v = editing?.video || state.config.video || {}
  return `<section class="panel"><div class="panel-head"><div><h2>${editing ? '编辑输入通道' : '新增输入通道'}</h2><p>通道拥有独立的采集、录像和网络输出配置</p></div><button id="channel-cancel" class="button">取消</button></div><div class="form-grid">${field('通道 ID', control('channel-id', editing?.id || `channel-${channels.length + 1}`))}${field('显示名称', control('channel-name', editing?.name || `输入通道 ${channels.length + 1}`))}${field('启用通道', `<label class="check"><input id="channel-enabled" type="checkbox" ${checked(editing?.enabled ?? true)}> 启用</label>`)}${field('视频设备', select('channel-device', v.device, [['', '自动选择'], ...state.videos.map(x => [x.path, `${x.path} · ${x.name}`])]))}${field('输入格式', select('channel-format', v.input_format || 'mjpeg', [['mjpeg', 'MJPEG'], ['yuyv422', 'YUYV 4:2:2'], ['h264', 'H.264']]))}${field('输出编码', select('channel-codec', v.codec || 'h264', [['h264', 'H.264'], ['h265', 'H.265']]))}${field('编码器', select('channel-encoder', v.encoder || 'auto', [['auto', '自动'], ['hardware', '硬件'], ['software', '软件']]))}${field('宽度', control('channel-width', v.width || 1920, 'number'))}${field('高度', control('channel-height', v.height || 1080, 'number'))}${field('帧率', control('channel-fps', v.fps || 30, 'number'))}${field('码率（kbps）', control('channel-bitrate', v.bitrate_kbps || 4000, 'number'))}</div><div class="right"><button id="channel-save" class="button primary">保存通道</button></div></section>`
}

async function saveChannel() {
  const value = id => document.querySelector(`#${id}`)?.value
  const id = value('channel-id')?.trim()
  if (!id) { state.message = '通道 ID 不能为空'; render(); return }
  const channels = state.config.channels || []
  const current = channels.find(channel => channel.id === state.editChannelId) || channels.find(channel => channel.id === id)
  const base = current || { video: { ...(state.config.video || {}) }, audio: { ...(state.config.audio || {}) }, recording: { ...(state.config.recording || {}) }, streaming: { ...(state.config.streaming || {}) } }
  const channel = { ...base, id, name: value('channel-name') || id, enabled: document.querySelector('#channel-enabled').checked, video: { ...base.video, device: value('channel-device') || null, input_format: value('channel-format'), codec: value('channel-codec'), encoder: value('channel-encoder'), width: +value('channel-width'), height: +value('channel-height'), fps: +value('channel-fps'), bitrate_kbps: +value('channel-bitrate') }, audio: { ...base.audio }, recording: { ...base.recording }, streaming: { ...base.streaming } }
  const next = current ? channels.map(item => item === current ? channel : item) : [...channels, channel]
  try { await api('/config', { method: 'PUT', body: JSON.stringify({ ...state.config, channels: next }) }); state.editChannelId = null; state.message = '通道配置已保存'; await load() } catch (error) { state.message = error.message; render() }
}

async function deleteChannel(id) {
  if (!window.confirm(`确定删除通道「${id}」吗？`)) return
  try { await api('/config', { method: 'PUT', body: JSON.stringify({ ...state.config, channels: (state.config.channels || []).filter(channel => channel.id !== id) }) }); state.message = '通道已删除'; await load() } catch (error) { state.message = error.message; render() }
}

function icon(name) {
  const paths = {
    overview: '<path d="m3 10 9-7 9 7v10H3Z"/><path d="M9 20v-7h6v7"/>',
    input: '<rect x="4" y="4" width="16" height="12" rx="2"/><path d="M8 21h8M12 16v5"/>',
    audio: '<rect x="9" y="3" width="6" height="12" rx="3"/><path d="M5 10a7 7 0 0 0 14 0M12 17v4M8 21h8"/>',
    encoding: '<rect x="5" y="5" width="14" height="14" rx="2"/><path d="M9 1v4M15 1v4M9 19v4M15 19v4M1 9h4M1 15h4M19 9h4M19 15h4M9 9h6v6H9Z"/>',
    rtsp: '<circle cx="12" cy="12" r="9"/><path d="m10 8 6 4-6 4Z"/>',
    rtmp: '<path d="M4 16V8M8 19V5M12 21V3M16 19V5M20 16V8"/>',
    onvif: '<circle cx="12" cy="5" r="3"/><circle cx="5" cy="18" r="3"/><circle cx="19" cy="18" r="3"/><path d="m10 8-3 7M14 8l3 7M8 18h8"/>',
    record: '<rect x="3" y="6" width="14" height="14" rx="2"/><path d="m17 10 4-2v10l-4-2M6 3h8"/>',
    play: '<circle cx="12" cy="12" r="9"/><path d="m10 8 6 4-6 4Z"/>',
    download: '<path d="M12 3v12m-5-5 5 5 5-5M4 15v6h16v-6"/>',
    trash: '<path d="M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7"/>',
    stop: '<rect x="6" y="6" width="12" height="12" rx="1"/>',
    files: '<path d="M3 7h7l2-3h9v16H3Z"/><path d="M3 10h18"/>',
    storage: '<path d="m5 4-3 12v4h20v-4L19 4ZM2 16h20"/><circle cx="17" cy="18" r=".5"/>',
    system: '<path d="M5 5h14v14H5ZM9 1v4M15 1v4M9 19v4M15 19v4M1 9h4M1 15h4M19 9h4M19 15h4"/><path d="M9 9h6v6H9Z"/>'
  }
  return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths[name] || paths.system}</svg>`
}
function nav() {
  const items = [['overview','概览'],['input','输入设备'],['encoding','编码设置'],['rtsp','RTSP'],['rtmp','RTMP'],['onvif','ONVIF'],['record','录像策略'],['files','文件管理'],['storage','存储管理'],['system','系统设置']]
  return `<aside class="sidebar"><div class="brand"><b>SB</b><strong>Stream Box</strong></div><nav>${items.map(([id,label]) => `<button data-page="${id}" class="nav ${state.page === id ? 'nav-active' : ''}" ${state.page === id ? 'aria-current="page"' : ''}><i>${icon(id)}</i>${label}</button>`).join('')}</nav></aside>`
}

function header(title, caption) { return `<header class="topbar"><div><small>${caption}</small><h1>${title}</h1></div><div class="top-status"><span>${new Date().toLocaleString('zh-CN')}</span><b class="badge ${state.status.runtime.status === 'running' ? 'badge-live' : ''}">${statusText(state.status.runtime.status)}</b></div></header>` }
function metric(label, value, detail) { return `<div class="panel metric"><span>${label}</span><strong>${value}</strong><small>${detail}</small></div>` }
function actionButton(id, text, kind = '') { return `<button id="${id}" class="button ${kind}">${text}</button>` }
function deviceList() { return state.videos.map(v => { const signal = v.signal_present == null ? '信号未知' : v.signal_present ? '信号正常' : '无信号'; const current = [v.current_format, v.current_resolution, v.current_frame_rate].filter(Boolean).join(' · '); return `<div class="device-row"><span class="dot ${v.signal_present === false ? '' : 'live'}"></span><div><b>${esc(v.name)}</b><small>${esc(v.path)} · ${esc(v.formats.join(', ') || '格式未知')}</small><small>${esc(signal)}${current ? ` · ${esc(current)}` : ''}</small></div><em>${esc(v.driver)}</em></div>` }).join('') || '<div class="empty">暂无视频设备</div>' }
function recordings(files = state.recordings) { return `<div class="table-wrap"><table><thead><tr><th>文件名</th><th>大小</th><th>修改时间</th><th class="file-actions-heading">操作</th></tr></thead><tbody>${files.map(file => `<tr><td><b>${esc(file.name)}</b><small>${file.name.endsWith('.mp4') ? 'MP4' : 'MPEG-TS'}</small></td><td>${bytes(file.bytes)}</td><td>${esc(new Date(file.modified).toLocaleString('zh-CN'))}</td><td><div class="file-actions"><button class="icon-button" data-play="${esc(file.name)}" title="预览播放" aria-label="预览播放 ${esc(file.name)}">${icon('play')}</button><button class="icon-button" data-download="${esc(file.name)}" title="下载" aria-label="下载 ${esc(file.name)}">${icon('download')}</button><button class="icon-button danger" data-delete="${esc(file.name)}" title="删除" aria-label="删除 ${esc(file.name)}">${icon('trash')}</button></div></td></tr>`).join('') || '<tr><td colspan="4" class="empty">暂无录像文件</td></tr>'}</tbody></table></div>` }

function updateRecordingStatus() {
  const running = !!state.status.runtime.recording
  document.querySelectorAll('[data-recording-status]').forEach(element => { element.classList.toggle('is-recording', running); element.querySelector('span').textContent = running ? '录制中' : '未开始录制' })
  const control = state.status.runtime.recording_control || 'policy'
  const mode = document.querySelector('[data-recording-control]')
  if (mode) mode.textContent = ({ policy: '按录像策略运行', manual_start: '手动开始 · 已覆盖录像策略', manual_stop: '手动停止 · 已覆盖录像策略' })[control]
  const start = document.querySelector('#start'), stop = document.querySelector('#stop'), policy = document.querySelector('#resume-policy')
  if (start) start.disabled = state.recordingBusy || (running && control === 'manual_start')
  if (stop) stop.disabled = state.recordingBusy || (!running && control === 'manual_stop')
  if (policy) policy.disabled = state.recordingBusy || control === 'policy'
}

async function recordingAction(path) {
  if (state.recordingBusy) return
  state.recordingBusy = true; updateRecordingStatus()
  try {
    await api(path, { method: 'POST' })
    state.status = await api('/status')
    showMessage(({ '/recording/start': '已手动开始录像', '/recording/stop': '已手动停止录像', '/recording/policy': '已恢复录像策略' })[path])
  } catch (error) { showMessage(error.message) }
  finally { state.recordingBusy = false; updateRecordingStatus() }
}

function filteredRecordings() { return state.recordings.filter(file => file.name.toLowerCase().includes(state.fileSearch.toLowerCase()) && (state.fileType === 'all' || file.name.endsWith(`.${state.fileType}`))) }
function bindRecordingFiles() {
  document.querySelectorAll('[data-play]').forEach(element => element.onclick = () => openRecordingPlayer(element.dataset.play))
  document.querySelectorAll('[data-download]').forEach(element => element.onclick = async () => { try { await downloadRecording(element.dataset.download) } catch (error) { showMessage(error.message) } })
  document.querySelectorAll('[data-delete]').forEach(element => element.onclick = async () => { if (!confirm(`删除录像 ${element.dataset.delete}？`)) return; try { await api(`/recordings/${encodeURIComponent(element.dataset.delete)}`, { method: 'DELETE' }); await load() } catch (error) { showMessage(error.message) } })
}
function updateFileList() { const list = document.querySelector('#recording-list'); if (list) { list.innerHTML = recordings(filteredRecordings()); bindRecordingFiles() } }
async function openRecordingPlayer(name) {
  const dialog = document.querySelector('#recording-player'), video = dialog.querySelector('video'), message = dialog.querySelector('[data-player-message]')
  const epoch = ++state.playbackEpoch
  dialog.querySelector('h2').textContent = name
  message.textContent = '正在加载录像…'; video.hidden = true; dialog.showModal()
  video.onerror = () => { message.textContent = '浏览器无法播放此视频编码，请下载后使用本地播放器打开'; video.hidden = true }
  video.onloadeddata = () => { message.textContent = ''; video.hidden = false }
  try {
    const result = await api(`/recordings/${encodeURIComponent(name)}/preview`, { method: 'POST', signal: AbortSignal.timeout(120000) })
    if (!dialog.open || epoch !== state.playbackEpoch) return
    video.src = `/api/recordings/${encodeURIComponent(name)}/preview?token=${encodeURIComponent(result.token)}`
    video.hidden = false
    try { await video.play() } catch (error) { if (error.name === 'NotAllowedError') message.textContent = '点击播放按钮开始预览' }
  } catch (error) { if (dialog.open && epoch === state.playbackEpoch) message.textContent = error.message }
}


async function downloadRecording(name) {
  const request = () => {
    const headers = {};
    const credentials = sessionStorage.getItem('streambox-auth');
    if (credentials) headers.authorization = `Basic ${credentials}`;
    return fetch(`/api/recordings/${encodeURIComponent(name)}/download`, { headers });
  };
  let response = await request();
  if (response.status === 401) { sessionStorage.removeItem('streambox-auth'); render(); throw new Error('请重新登录') }
  if (!response.ok) throw new Error('录像下载失败');
  const blob = await response.blob();
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

function overview() {
  const v = state.config.video || {}, a = state.config.audio || {}
  const input = configuredVideo(), audio = state.audios.find(device => device.name === a.device)
  const row = (label, value) => `<div><span>${label}</span><strong>${esc(value)}</strong></div>`
  const videoInfo = row('设备名称',input?.name || '未检测到设备') + row('设备地址',input?.path || v.device || '—') + row('输入分辨率／帧率',`${v.width || '—'} × ${v.height || '—'}@${v.fps || '—'}fps`) + row('输入画面格式',videoFormatText(v.input_format)) + row('编码格式',videoFormatText(v.codec))
  const audioInfo = row('设备名称',a.enabled ? audio?.description || '未选择设备' : '已关闭') + row('设备地址',a.enabled ? audio?.path || a.device || '—' : '—') + row('输入格式',a.enabled && a.device ? `${a.sample_rate} Hz · ${a.input_format || 'S16_LE'}` : '—') + row('采样率',a.enabled && a.device ? `${a.sample_rate} Hz` : '—') + row('编码格式',a.enabled ? audioCodecText(a.codec) : '—')
  return `<div class="overview-grid overview-top"><section class="panel preview-panel"><div class="panel-head"><h2>输入预览</h2></div>${previewBox()}</section><section class="panel source-panel"><div class="panel-head"><h2>输入源信息</h2></div><div class="source-columns"><div><h3>视频</h3>${videoInfo}</div><div><h3>音频</h3>${audioInfo}</div></div></section></div>
  <div class="overview-grid overview-mid"><section class="panel"><div class="panel-head"><h2>系统资源</h2></div>${resourceCards()}</section><section class="panel"><div class="panel-head"><h2>服务状态</h2></div><div class="service-list">${['rtsp','rtmp','onvif'].map(key => `<div class="service-row"><span class="service-icon">${icon(key)}</span><b>${key.toUpperCase()}</b><em class="${state.status.outputs?.[key] ? 'ok' : 'off'}"><i></i>${state.status.outputs?.[key] ? '运行中' : '已停止'}</em></div>`).join('')}</div></section></div>`
}

function recordingPage() {
  const r=state.config.recording || {}
  const running=!!state.status.runtime.recording
  return `<section class="panel manual-recording"><div class="panel-head"><h2>手动控制</h2></div><div class="manual-recording-body"><div class="manual-recording-state"><b data-recording-status class="${running ? 'is-recording' : ''}"><i></i><span>${running ? '录制中' : '未开始录制'}</span></b><small data-recording-control></small></div><div class="quick-actions"><button id="start" class="button primary">${icon('play')}开始录像</button><button id="stop" class="button danger">${icon('stop')}停止录像</button><button id="resume-policy" class="button">恢复策略</button></div></div><p class="panel-note">手动控制优先于录像策略，点击“恢复策略”或重启服务后恢复自动控制。</p></section>` + settingsPanel('录像策略设置',`${field('录像格式',select('recording-container',r.container || 'mpegts',[['mpegts','MPEG-TS'],['mp4','MP4']]))}${field('分段时长（秒）',control('recording-segment',r.segment_seconds ?? 600,'number'))}${toggle('recording-auto','启动自动录像',r.auto_start)}${toggle('recording-schedule','定时录像',r.schedule_enabled)}<div id="recording-schedule-options" class="recording-schedule-options" ${r.schedule_enabled ? '' : 'hidden'}>${field('开始时间',control('schedule-start',r.schedule_start || '08:00','time'))}${field('结束时间',control('schedule-stop',r.schedule_stop || '18:00','time'))}</div>`)
}

function channelsPage() { const configured = state.config.channels || []; return `${state.editChannelId || state.editChannelId === '' ? channelEditor() : ''}<section class="panel"><div class="panel-head"><div><h2>多路输入</h2><p>独立配置每路 V4L2 采集、编码、录像和网络输出</p></div><div class="quick-actions"><button id="channel-new" class="button primary">＋ 新增通道</button><button id="refresh" class="button">↻ 刷新</button></div></div><div class="grid grid-cols-1 md:grid-cols-2 gap-4">${state.channels.map(channel => `<div class="panel border border-slate-800"><div class="panel-head"><div><h2>${esc(channel.name)}</h2><p>${esc(channel.id)} · ${esc(channel.video_device || '自动选择设备')}</p></div><b class="badge ${channel.recording ? 'badge-live' : ''}">${channel.recording ? '运行中' : '已停止'}</b></div><div class="info-list"><div>RTSP <b>${channel.outputs?.rtsp ? '运行中' : '未启动'}</b></div><div>RTMP <b>${channel.outputs?.rtmp_connected ? '推流中' : channel.outputs?.rtmp ? '重连中' : '未启动'}</b></div><div>信号设备 <b>${esc(channel.video_device || '-')}</b></div></div><div class="quick-actions"><button class="button" data-channel-edit="${esc(channel.id)}">编辑</button><button class="button primary" data-channel-start="${esc(channel.id)}">▶ 启动</button><button class="button danger" data-channel-stop="${esc(channel.id)}">■ 停止</button><button class="link danger" data-channel-delete="${esc(channel.id)}">删除</button></div></div>`).join('') || '<div class="empty">暂无启用通道，点击“新增通道”开始配置。</div>'}</div><div class="hint">${configured.filter(channel => !channel.enabled).length ? `已禁用通道：${configured.filter(channel => !channel.enabled).map(channel => esc(channel.name)).join('、')}` : '配置会即时写入并同步到运行时。'}</div></section>` }

function webUpgradePanel() {
  return `<section class="panel"><div class="panel-head"><h2>网页升级</h2></div><div class="quick-actions"><input id="ota-file" type="file" accept=".elf,application/octet-stream" class="control"><button id="ota-upload" class="button primary">上传固件</button><button id="ota-apply" class="button danger">应用并重启</button></div></section>`
}
function securityPanel() {
  const s=state.config.security || {}
  return settingsPanel('访问安全',`${field('用户名',control('security-user',s.username || 'admin'))}${field('密码',control('security-pass',s.password || '','password'))}`)
}
function systemPage() {
  const logPanel=`<section class="panel"><div class="panel-head"><h2>运行日志</h2></div><div class="info-list">${state.logs.slice(-8).reverse().map(log=>`<div><span class="badge">${esc(log.level)}</span><small>${esc(log.timestamp)}</small><b>${esc(log.message)}</b></div>`).join('') || '<div class="empty">暂无运行日志</div>'}</div></section>`
  return `${logPanel}${webUpgradePanel()}${securityPanel()}`
}

const pageIntro = (title, desc, action = '') => action ? `<div class="page-actions">${action}</div>` : ''
const toggle = (id, label, value = true) => `<label class="toggle-line"><span>${label}</span><input id="${id}" type="checkbox" ${checked(value)}><i></i></label>`
const statusPill = (text, good = true) => `<span class="status-pill ${good ? 'good' : 'muted'}"><i></i>${text}</span>`
const radio = (label, active = false) => `<label class="radio-option"><i class="${active ? 'active' : ''}"></i>${label}</label>`

function deviceMetadata(device, audio = false) {
  return `<dl class="device-properties"><div><dt>设备名称</dt><dd>${esc(audio ? device.description : device.name)}</dd></div><div><dt>设备地址</dt><dd>${esc(device.path || device.name)}</dd></div><div><dt>USB VID:PID</dt><dd>${esc(device.usb_vid_pid || '—')}</dd></div><div><dt>USB 端口号</dt><dd>${esc(device.usb_port || '—')}</dd></div></dl>`
}
function inputDevicesPage() {
  const videoCard = device => `<article class="panel input-device"><div class="panel-head"><div class="device-title"><span class="device-symbol">${icon('input')}</span><h2>${esc(device.name)}</h2></div>${statusPill('视频输入', true)}</div>${deviceMetadata(device)}<div class="device-capabilities"><h3>视频输入预设</h3><div class="table-wrap"><table><thead><tr><th>分辨率</th><th>帧率</th><th>视频格式</th></tr></thead><tbody>${(device.modes || []).map(mode => `<tr><td>${esc(mode.resolution.replace('x',' × '))}</td><td>${mode.frame_rate} fps</td><td>${esc(videoFormatText(mode.format))}</td></tr>`).join('') || '<tr><td colspan="3" class="empty">未获取到视频输入能力</td></tr>'}</tbody></table></div></div></article>`
  const audioCard = device => `<article class="panel input-device"><div class="panel-head"><div class="device-title"><span class="device-symbol">${icon('audio')}</span><h2>${esc(device.description)}</h2></div>${statusPill('音频输入', device.available)}</div>${deviceMetadata(device,true)}<div class="device-capabilities"><h3>音频输入预设</h3><div class="table-wrap"><table><thead><tr><th>采样率</th><th>输入格式</th></tr></thead><tbody>${(device.modes || []).map(mode => `<tr><td>${mode.sample_rate} Hz</td><td>${esc(mode.format)}</td></tr>`).join('') || '<tr><td colspan="2" class="empty">未获取到音频输入能力</td></tr>'}</tbody></table></div></div></article>`
  return `<div class="page-actions"><button id="refresh" class="button primary">刷新设备</button></div><div class="device-section-heading"><h2>视频输入设备</h2><span>${state.videos.length} 个设备</span></div><section class="input-device-grid">${state.videos.map(videoCard).join('') || '<div class="panel empty">未检测到视频输入设备</div>'}</section><div class="device-section-heading"><h2>音频输入设备</h2><span>${state.audios.length} 个设备</span></div><section class="input-device-grid">${state.audios.map(audioCard).join('') || '<div class="panel empty">未检测到音频输入设备</div>'}</section>`
}
const audioCodecText = codec => ({ aac: 'AAC', g711_alaw: 'G.711 A-law', g711_ulaw: 'G.711 μ-law' })[codec] || '—'
function encodingPage() {
  const v = state.config.video || {}, a = state.config.audio || {}, encoders = state.status.media.encoders || []
  return `<section class="panel encoding-settings"><div class="encoding-tabs" role="tablist" aria-label="编码参数"><span class="encoding-tab-indicator ${state.encodingTab === 'audio' ? 'at-audio' : ''}"></span><button id="video-tab" data-encoding-tab="video" role="tab" aria-selected="${state.encodingTab === 'video'}" aria-controls="video-settings" tabindex="${state.encodingTab === 'video' ? '0' : '-1'}">视频</button><button id="audio-tab" data-encoding-tab="audio" role="tab" aria-selected="${state.encodingTab === 'audio'}" aria-controls="audio-settings" tabindex="${state.encodingTab === 'audio' ? '0' : '-1'}">音频</button></div>
  <div id="video-settings" role="tabpanel" aria-labelledby="video-tab" ${state.encodingTab === 'video' ? '' : 'hidden'}><div class="settings-form horizontal-form">
  ${field('视频设备',select('video-device-main',v.device || '',[['','自动选择'],...state.videos.map(device=>[device.path,`${device.path} ${device.name}`]),...(v.device && !state.videos.some(device=>device.path===v.device) ? [[v.device,`${v.device}（设备未连接）`]] : [])]))}
  ${field('输入预设',select('video-mode-main','',[]))}
  ${field('编码器',select('video-encoder-main',v.encoder === 'hardware' ? 'v4l2m2m' : v.encoder || 'auto',encoders.map(encoder=>[encoder.id,encoder.name])))}
  ${field('视频编码',select('video-codec-main',v.codec || 'h264',[]))}
  ${field('视频码率（kbps）',control('video-bitrate-main',v.bitrate_kbps || 4000,'number'))}
  <div class="software-options" id="software-options" ${v.encoder === 'software' ? '' : 'hidden'}>${field('编码预设',select('video-preset-main',v.preset || 'veryfast',[['ultrafast','极快（Ultrafast）'],['veryfast','快速（Veryfast）'],['medium','均衡（Medium）'],['slow','质量优先（Slow）']]))}${field('编码 Profile',select('video-profile-main',v.profile || '',[['','自动'],...(v.codec === 'h265' ? [['main','Main']] : [['baseline','Baseline'],['main','Main'],['high','High']])]))}${field('编码 Level',select('video-level-main',v.level || '',[['','自动'],['3.1','3.1'],['4.0','4.0'],['4.1','4.1'],['5.0','5.0']]))}</div>
  </div></div>
  <div id="audio-settings" role="tabpanel" aria-labelledby="audio-tab" ${state.encodingTab === 'audio' ? '' : 'hidden'}><div class="settings-form horizontal-form">${toggle('audio-enabled-main','启用音频',a.enabled)}
  <div id="audio-parameters" class="audio-parameters" ${a.enabled ? '' : 'hidden'}>
  ${field('音频设备',select('audio-device-main',a.device || '',[['','请选择音频设备'],...state.audios.map(device=>[device.name,`${device.path || device.name} ${device.description}`]),...(a.device && !state.audios.some(device=>device.name===a.device) ? [[a.device,`${a.device}（设备未连接）`]] : [])]))}
  ${field('输入预设',select('audio-mode-main','',[]))}
  ${field('音频编码',select('audio-codec-main',a.codec || 'aac',[['aac','AAC'],['g711_alaw','G.711 A-law'],['g711_ulaw','G.711 μ-law']]))}
  ${field('音频码率（kbps）',control('audio-bitrate-main',a.codec === 'aac' ? a.bitrate_kbps || 128 : 64,'number'))}
  </div></div></div><div class="encoding-footer"><button id="save" class="button primary">应用设置</button></div></section>`
}

function settingsPanel(title, fields) {
  return `<section class="panel encoding-settings service-settings"><div class="panel-head"><h2>${title}</h2></div><div class="settings-form horizontal-form">${fields}</div><div class="encoding-footer"><button id="save" class="button primary">应用设置</button></div></section>`
}
function serviceAuthentication(prefix, config) {
  const enabled=!!(config.username && config.password)
  return `${toggle(`${prefix}-auth-enabled`,'客户端认证',enabled)}<div id="${prefix}-auth-options" class="service-auth-options" ${enabled ? '' : 'hidden'}>${field('用户名',control(`${prefix}-user`,config.username || ''))}${field('密码',control(`${prefix}-pass`,config.password || '','password'))}</div>`
}
function rtspPage() {
  const s=state.config.streaming?.rtsp || {}
  return settingsPanel('RTSP 服务设置',`${toggle('rtsp-enabled','启用 RTSP 服务',s.enabled)}${field('服务端口',control('rtsp-port',s.port || 8554,'number'))}${field('流路径',control('rtsp-path',s.path || '/camera/main'))}${serviceAuthentication('rtsp',s)}`)
}
function rtmpPage() {
  const s=state.config.streaming?.rtmp || {}
  return settingsPanel('推流服务设置',`${toggle('rtmp-enabled','启用 RTMP 推流',s.enabled)}${field('推流地址',control('rtmp-url',s.url || ''))}${field('重试间隔（秒）',control('rtmp-reconnect',s.reconnect_seconds ?? 5,'number'))}`)
}
function onvifPage() {
  const s=state.config.streaming?.onvif || {}
  return settingsPanel('ONVIF 服务设置',`${toggle('onvif-enabled','启用 ONVIF 服务',s.enabled)}${field('设备名称',control('onvif-name',s.name || 'StreamBox'))}${field('服务端口',control('onvif-port',s.port || 8000,'number'))}${serviceAuthentication('onvif',s)}`)
}

function filesPage() {
  return `<section class="panel files-panel"><div class="file-toolbar"><input id="file-search" class="control" type="search" value="${esc(state.fileSearch)}" placeholder="搜索文件名或时间…" aria-label="搜索文件名或时间"><label class="file-type-label">文件类型 ${select('file-type',state.fileType,[['all','全部'],['mp4','MP4'],['ts','MPEG-TS']])}</label><button id="refresh" class="button">↻ 刷新</button><button id="file-export" class="button primary">${icon('download')}导出列表</button></div><div id="recording-list">${recordings(filteredRecordings())}</div></section><dialog id="recording-player" aria-labelledby="player-title"><div class="player-head"><h2 id="player-title"></h2><button class="icon-button" id="player-close" aria-label="关闭播放器">×</button></div><video controls playsinline preload="metadata" hidden></video><p data-player-message role="status"></p></dialog>`
}

function storagePage() {
  return `${pageIntro('磁盘管理','查看真实磁盘、挂载状态和可用空间。')}
  <section class="panel"><div class="panel-head"><h2>存储设备列表</h2><button id="refresh" class="button">刷新状态</button></div><div class="disk-list">${state.storage.map(d => { const used = percent(d.used_bytes,d.total_bytes); return `<div class="disk-row"><span class="disk-icon">${icon('storage')}</span><div class="disk-main"><div class="disk-name"><b>${esc(d.name)}</b><span class="status-pill ${d.mounted ? 'good' : 'muted'}"><i></i>${d.mounted ? (d.read_only ? '只读' : '已挂载') : '未挂载'}</span>${d.protected ? '<span class="tag">系统保护</span>' : ''}</div><div class="disk-stats"><span>设备 <b>${esc(d.device)}</b></span><span>挂载点 <b>${esc(d.mount_point || '—')}</b></span><span>文件系统 <b>${esc(d.filesystem || '未格式化')}</b></span><span>总容量 <b>${sizeText(d.total_bytes)}</b></span><span>已用 <b>${sizeText(d.used_bytes)}</b></span><span>可用 <b>${sizeText(d.available_bytes)}</b></span><div class="disk-progress"><i style="width:${used || 0}%"></i></div><b>${used == null ? '—' : `${used}%`}</b></div></div><div class="disk-actions"><button class="button" data-smart="${esc(d.device)}">查看 SMART</button><button class="button" data-disk-action="${d.mounted ? 'unmount' : 'mount'}" data-device="${esc(d.device)}" ${d.protected ? 'disabled' : ''}>${d.mounted ? '卸载' : '挂载'}</button><button class="button danger" data-disk-action="format" data-device="${esc(d.device)}" ${!d.can_format ? 'disabled' : ''}>格式化</button></div></div>` }).join('') || '<div class="empty">未发现物理磁盘</div>'}</div></section>
  <div class="two-col-layout"><section class="panel"><div class="panel-head"><h2>录像存储位置</h2></div><div class="settings-form">${field('存储设备',select('record-storage-device',state.storage.filter(d=>d.mounted && state.config.recording?.directory?.startsWith(d.mount_point)).sort((a,b)=>b.mount_point.length-a.mount_point.length)[0]?.device || '',state.storage.filter(d=>d.mounted && !d.read_only).map(d=>[d.device,`${d.name} (${d.mount_point})`])))}${field('保存目录',control('recording-directory',state.config.recording?.directory || '/var/lib/streambox/recordings'))}</div><div class="right"><button id="save" class="button primary">保存设置</button></div></section><section class="panel"><div class="panel-head"><h2>自动清理设置</h2></div><div class="settings-form">${toggle('storage-loop','循环录像',state.config.recording?.loop_recording)}${field('最低保留空间 (MB)',control('storage-free',Math.round((state.config.recording?.min_free_bytes || 0)/1048576),'number'))}</div><p class="panel-note">可用空间低于设定值时，循环录像会清理最早的录像文件。</p></section></div>
  <p class="panel-note">系统盘及服务数据所在盘已保护。格式化会清除目标分区上的全部数据；请先卸载分区。</p><dialog id="disk-dialog"><form method="dialog"><h2 id="disk-dialog-title"></h2><p id="disk-dialog-text"></p><label id="disk-confirm-label" class="field"><span>输入设备路径确认</span><input class="control" id="disk-confirm" autocomplete="off"></label><p id="disk-dialog-error" role="alert"></p><div class="dialog-actions"><button class="button" value="cancel">取消</button><button type="button" id="disk-execute" class="button danger">确认操作</button></div></form></dialog><dialog id="smart-dialog"><form method="dialog"><h2>磁盘 SMART 状态</h2><pre id="smart-content"></pre><button class="button">关闭</button></form></dialog>`
}


function loginPage() {
  return `<main class="login-screen"><div class="login-art"><div class="login-brand"><b>SB</b><strong>Stream Box</strong><p>USB 摄像头 / HDMI 采集盒管理</p></div><div class="device-hero"><div class="device-light"></div><span>Stream Box</span></div></div><form id="login-form" class="login-card"><h1>欢迎使用 Stream Box</h1><p class="login-subtitle">请登录设备管理界面</p><label class="login-field"><span>用户名</span><input id="login-user" class="control" value="${esc(localStorage.getItem('streambox-user') || 'admin')}" required autocomplete="username"></label><label class="login-field"><span>密码</span><div class="password-wrap"><input id="login-pass" class="control" type="password" placeholder="请输入密码" required autocomplete="current-password"><button type="button" id="login-toggle" aria-label="显示或隐藏密码">◉</button></div></label><label class="remember"><input id="login-remember" type="checkbox" ${checked(!!localStorage.getItem('streambox-user'))}> 记住用户名</label><button id="login-submit" type="submit" class="button primary login-submit">登 录 <span>→</span></button><p class="login-help" role="alert">${esc(state.message)}</p></form></main>`
}


function render() {
  if (!credentials() || new URLSearchParams(location.search).get('login') === '1') {
    document.querySelector('#app').innerHTML = loginPage()
    document.querySelector('#login-toggle').onclick = () => { const input=document.querySelector('#login-pass'); input.type=input.type === 'password' ? 'text' : 'password' }
    document.querySelector('#login-form').onsubmit = async event => {
      event.preventDefault()
      const user=document.querySelector('#login-user').value, pass=document.querySelector('#login-pass').value, remember=document.querySelector('#login-remember').checked, button=document.querySelector('#login-submit')
      button.disabled=true
      try {
        const response=await fetch('/api/login',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({username:user,password:pass}),signal:AbortSignal.timeout(10000)})
        if (!response.ok) throw new Error(response.status===401 ? '用户名或密码错误' : '登录服务暂时不可用')
        sessionStorage.setItem('streambox-auth',encodeCredentials(user,pass)); sessionStorage.setItem('streambox-user',user)
        if (remember) localStorage.setItem('streambox-user',user); else localStorage.removeItem('streambox-user')
        history.replaceState(null,'','/'); state.message=''; await load()
      } catch(error) { document.querySelector('.login-help').textContent=error.message; button.disabled=false }
    }
    return
  }
  const pages={overview,input:inputDevicesPage,encoding:encodingPage,rtsp:rtspPage,rtmp:rtmpPage,onvif:onvifPage,record:recordingPage,files:filesPage,storage:storagePage,system:systemPage}
  document.querySelector('#app').innerHTML=`${nav()}<main class="main"><header class="topbar"><div class="topbar-title"><button class="icon-button" id="menu-toggle" aria-label="折叠导航">☰</button><h1>${esc(({overview:"设备概览",input:"输入设备",encoding:"编码设置",rtsp:"RTSP",rtmp:"RTMP",onvif:"ONVIF",record:"录像策略",files:"文件管理",storage:"存储管理",system:"系统设置"})[state.page])}</h1></div><div class="top-status"><span>中文</span><button class="user-button" id="logout" title="退出登录">${esc(sessionStorage.getItem('streambox-user') || 'admin')} · 退出</button></div></header><div class="content">${state.message ? `<div class="notice" role="status">${esc(state.message)}<button id="close-message" aria-label="关闭提示">×</button></div>` : ''}${pages[state.page]()}</div></main>`
  document.querySelector('#logout').onclick=()=>{sessionStorage.removeItem('streambox-auth');sessionStorage.removeItem('streambox-user');state.dirty=false;state.message='';if(state.previewUrl)URL.revokeObjectURL(state.previewUrl);state.previewUrl='';render()}
  document.querySelector('#menu-toggle').onclick=()=>document.body.classList.toggle('nav-collapsed')
  document.querySelectorAll('[data-page]').forEach(element=>element.onclick=()=>{state.page=element.dataset.page;state.editRevision++;state.previewExpanded=false;document.body.classList.remove('preview-is-expanded');state.dirty=false;state.message='';render()})
  document.querySelector('#close-message')?.addEventListener('click',()=>{state.message='';document.querySelector('.notice')?.remove()})
  document.querySelectorAll('#save,[data-save]').forEach(element=>element.onclick=save)
  document.querySelector('#start')?.addEventListener('click',()=>recordingAction('/recording/start'))
  document.querySelector('#stop')?.addEventListener('click',()=>recordingAction('/recording/stop'))
  document.querySelector('#resume-policy')?.addEventListener('click',()=>recordingAction('/recording/policy'))
  document.querySelector('#outputs-start')?.addEventListener('click',()=>action('/outputs/start'))
  document.querySelector('#outputs-stop')?.addEventListener('click',()=>action('/outputs/stop'))
  document.querySelector('#refresh')?.addEventListener('click',()=>load())
  document.querySelector('#preview-refresh')?.addEventListener('click',loadPreview)
  document.querySelector('[data-fullscreen]')?.addEventListener('click', togglePreviewExpanded)
  document.querySelectorAll('.content input:not(#file-search),.content select:not(#file-type)').forEach(element=>element.addEventListener('input',()=>{state.dirty=true;state.editRevision++}))
  document.querySelectorAll('[data-encoding-tab]').forEach(button => {
    button.onclick = () => switchEncodingTab(button.dataset.encodingTab)
    button.onkeydown = event => { if (['ArrowLeft','ArrowRight','Home','End'].includes(event.key)) { event.preventDefault(); const tab = event.key === 'Home' || event.key === 'ArrowLeft' ? 'video' : 'audio'; switchEncodingTab(tab); document.querySelector(`[data-encoding-tab="${tab}"]`).focus() } }
  })
  document.querySelector('#video-device-main')?.addEventListener('change',()=>syncVideoModeOptions(true))
  document.querySelector('#video-encoder-main')?.addEventListener('change',syncEncoderOptions)
  document.querySelector('#video-codec-main')?.addEventListener('change',syncSoftwareOptions)
  document.querySelector('#audio-device-main')?.addEventListener('change',()=>syncAudioModeOptions(true))
  document.querySelector('#audio-enabled-main')?.addEventListener('change',event=>{document.querySelector('#audio-parameters').hidden=!event.target.checked})
  document.querySelector('#audio-codec-main')?.addEventListener('change',syncAudioBitrate)
  document.querySelector('#recording-schedule')?.addEventListener('change',event=>{document.querySelector('#recording-schedule-options').hidden=!event.target.checked})
  for (const prefix of ['rtsp','onvif']) document.querySelector(`#${prefix}-auth-enabled`)?.addEventListener('change',event=>{document.querySelector(`#${prefix}-auth-options`).hidden=!event.target.checked})
  document.querySelector('#record-storage-device')?.addEventListener('change',event=>{const disk=state.storage.find(d=>d.device===event.target.value);if(disk)document.querySelector('#recording-directory').value=disk.mount_point==='/' ? '/var/lib/streambox/recordings' : `${disk.mount_point}/recordings`})
  document.querySelectorAll('[data-disk-action]').forEach(element=>element.onclick=()=>openDiskDialog(element.dataset.device,element.dataset.diskAction))
  document.querySelectorAll('[data-smart]').forEach(element=>element.onclick=async()=>{const dialog=document.querySelector('#smart-dialog'),content=document.querySelector('#smart-content');content.textContent='正在读取…';dialog.showModal();try{const data=await api(`/storage/smart?device=${encodeURIComponent(element.dataset.smart)}`);content.textContent=JSON.stringify(data,null,2)}catch(error){content.textContent=error.message}})
  bindRecordingFiles(); updateRecordingStatus()
  document.querySelector('#file-search')?.addEventListener('input', event => { state.fileSearch = event.target.value; updateFileList() })
  document.querySelector('#file-type')?.addEventListener('change', event => { state.fileType = event.target.value; updateFileList() })
  document.querySelector('#file-export')?.addEventListener('click', () => {
    const csv = '\uFEFF' + [['文件名','字节数','修改时间'], ...filteredRecordings().map(file => [file.name,file.bytes,file.modified])].map(row => row.map(value => `"${String(value ?? '').replaceAll('"','""')}"`).join(',')).join('\r\n')
    const url = URL.createObjectURL(new Blob([csv], { type: 'text/csv;charset=utf-8' })), link = document.createElement('a'); link.href = url; link.download = '录像文件列表.csv'; link.click(); URL.revokeObjectURL(url)
  })
  const player = document.querySelector('#recording-player')
  document.querySelector('#player-close')?.addEventListener('click', () => player.close())
  player?.addEventListener('close', () => { state.playbackEpoch++; const video=player.querySelector('video'); video.pause(); video.removeAttribute('src'); video.load() })
  document.querySelector('#ota-upload')?.addEventListener('click',async()=>{const file=document.querySelector('#ota-file')?.files[0];if(!file)return showMessage('请选择固件文件');try{await api('/system/ota',{method:'PUT',body:file,headers:{'content-type':'application/octet-stream'}});showMessage('固件已上传，可应用更新')}catch(error){showMessage(error.message)}})
  document.querySelector('#ota-apply')?.addEventListener('click',()=>action('/system/ota/apply'))
  syncVideoCapabilityControls(); syncEncoderOptions(); syncAudioModeOptions(); syncAudioBitrate()
  if (state.page === 'overview') loadPreview()
}
function showMessage(message) { state.message=message; const existing=document.querySelector('.notice'); if(existing)existing.remove(); const notice=document.createElement('div');notice.className='notice';notice.setAttribute('role','status');notice.textContent=message;document.querySelector('.content')?.prepend(notice) }
function openDiskDialog(device,action) {
  const dialog=document.querySelector('#disk-dialog'),confirmInput=document.querySelector('#disk-confirm'),error=document.querySelector('#disk-dialog-error')
  const label={mount:'挂载',unmount:'卸载',format:'格式化'}[action]
  document.querySelector('#disk-dialog-title').textContent=`${label} ${device}`
  document.querySelector('#disk-dialog-text').textContent=action==='format' ? '此操作将永久删除分区内全部数据，请确认设备路径。' : `确认${label}这个设备？`
  document.querySelector('#disk-confirm-label').hidden=action!=='format';confirmInput.value='';error.textContent='';dialog.showModal()
  document.querySelector('#disk-execute').onclick=async()=>{
    const button=document.querySelector('#disk-execute');button.disabled=true
    try{await api('/storage/action',{method:'POST',signal:AbortSignal.timeout(70000),body:JSON.stringify({device,action,confirmation:confirmInput.value})});dialog.close();state.dirty=false;state.message=`${label}成功`;await load()}
    catch(err){error.textContent=err.message}finally{button.disabled=false}
  }
}


const videoFormatLabels = { mjpeg: 'MJPEG', yuyv422: 'YUYV 4:2:2', h264: 'H.264' }

function switchEncodingTab(tab) {
  state.encodingTab = tab
  document.querySelectorAll('[data-encoding-tab]').forEach(button => { const active = button.dataset.encodingTab === tab; button.setAttribute('aria-selected',active); button.tabIndex=active ? 0 : -1 })
  document.querySelector('.encoding-tab-indicator')?.classList.toggle('at-audio',tab === 'audio')
  document.querySelector('#video-settings').hidden = tab !== 'video'
  document.querySelector('#audio-settings').hidden = tab !== 'audio'
}
function togglePreviewExpanded() {
  state.previewExpanded = !state.previewExpanded
  const box = document.querySelector('.preview-box'), button = document.querySelector('[data-fullscreen]')
  if (!box || !button) return
  box.classList.toggle('preview-expanded',state.previewExpanded)
  document.body.classList.toggle('preview-is-expanded',state.previewExpanded)
  document.querySelectorAll('.sidebar,.topbar,.source-panel,.overview-mid,.preview-panel .panel-head').forEach(element=>{element.inert=state.previewExpanded})
  button.focus()
  button.textContent = state.previewExpanded ? '↙ 缩回' : '⛶ 放大'
  button.setAttribute('aria-label',state.previewExpanded ? '缩回预览' : '放大预览')
  button.setAttribute('aria-expanded',state.previewExpanded)
  if (state.previewExpanded) { box.setAttribute('role','dialog'); box.setAttribute('aria-modal','true'); box.setAttribute('aria-label','输入预览') }
  else { box.removeAttribute('role'); box.removeAttribute('aria-modal'); box.removeAttribute('aria-label') }
}
document.addEventListener('keydown',event=>{if(!state.previewExpanded)return;if(event.key === 'Escape')togglePreviewExpanded();if(event.key === 'Tab'){event.preventDefault();document.querySelector('[data-fullscreen]')?.focus()}})
function syncVideoModeOptions(reset = false) {
  const element = document.querySelector('#video-mode-main'), deviceSelect = document.querySelector('#video-device-main')
  if (!element || !deviceSelect || (!reset && element.dataset.initialized)) return
  const device = state.videos.find(item=>item.path === deviceSelect.value) || (!deviceSelect.value ? state.videos[0] : null)
  const v = state.config.video
  const current = reset ? '' : videoModeKey({resolution:`${v.width}x${v.height}`,frame_rate:v.fps,format:v.input_format})
  const modes = device?.modes || []
  element.innerHTML = modes.length ? modes.map(mode=>`<option value="${esc(videoModeKey(mode))}">${esc(videoModeText(mode))}</option>`).join('') : '<option value="">未获取到视频输入能力</option>'
  if (modes.some(mode=>videoModeKey(mode) === current)) element.value = current
  element.disabled = !modes.length; element.dataset.initialized = '1'
}
function syncEncoderOptions() {
  const encoder = document.querySelector('#video-encoder-main'), codec = document.querySelector('#video-codec-main')
  if (!encoder || !codec) return
  const current = codec.value || state.config.video.codec
  const codecs = state.status.media.encoders?.find(item => item.id === encoder.value)?.codecs || []
  codec.innerHTML = codecs.length ? codecs.map(value=>`<option value="${esc(value)}">${esc(videoFormatText(value))}</option>`).join('') : '<option value="">没有可用编码</option>'
  if (codecs.includes(current)) codec.value = current
  codec.disabled = !codecs.length
  syncSoftwareOptions()
}
function syncSoftwareOptions() {
  const options=document.querySelector('#software-options'), profile=document.querySelector('#video-profile-main')
  if (!options || !profile) return
  options.hidden=document.querySelector('#video-encoder-main').value!=='software'
  const profiles=document.querySelector('#video-codec-main').value==='h265' ? [['','自动'],['main','Main']] : [['','自动'],['baseline','Baseline'],['main','Main'],['high','High']]
  setCapabilityOptions(profile,profiles,profile.value)
}
function syncAudioModeOptions(reset = false) {
  const element = document.querySelector('#audio-mode-main'), deviceSelect = document.querySelector('#audio-device-main')
  if (!element || !deviceSelect || (!reset && element.dataset.initialized)) return
  const a = state.config.audio, modes = state.audios.find(item => item.name === deviceSelect.value)?.modes || []
  const current = reset ? '' : `${a.sample_rate}|${a.input_format || 'S16_LE'}`
  element.innerHTML = modes.length ? modes.map(mode=>`<option value="${esc(audioModeKey(mode))}">${mode.sample_rate} Hz · ${esc(mode.format)}</option>`).join('') : '<option value="">未获取到音频输入能力</option>'
  if (modes.some(mode=>audioModeKey(mode) === current)) element.value = current
  element.disabled = !modes.length; element.dataset.initialized = '1'
}
function syncAudioBitrate() {
  const codec = document.querySelector('#audio-codec-main'), bitrate = document.querySelector('#audio-bitrate-main')
  if (!codec || !bitrate) return
  if (codec.value !== 'aac') { if (!bitrate.disabled) bitrate.dataset.aacBitrate = bitrate.value; bitrate.value=64; bitrate.disabled=true }
  else if (bitrate.disabled) { bitrate.disabled=false; bitrate.value=bitrate.dataset.aacBitrate || state.config.audio.bitrate_kbps || 128 }
}

function selectedVideoDevice() {
  const path = document.querySelector('#video-device')?.value || state.config.video?.device
  return state.videos.find(device => device.path === path) || state.videos[0]
}

function setCapabilityOptions(selectElement, options, current) {
  if (!selectElement) return
  selectElement.replaceChildren(...options.map(([value, label]) => { const option = document.createElement('option'); option.value = value; option.textContent = label; return option }))
  selectElement.value = options.some(([value]) => value === current) ? current : (options[0]?.[0] || '')
}

function ensureResolutionControl() {
  const width = document.querySelector('#video-width')
  const height = document.querySelector('#video-height')
  if (!width || !height) return null
  let resolution = document.querySelector('#video-resolution')
  if (!resolution) {
    const fieldElement = document.createElement('label')
    fieldElement.className = 'field capability-resolution-field'
    fieldElement.innerHTML = '<span>分辨率</span><select id="video-resolution" class="control"></select>'
    width.closest('.field').parentElement.insertBefore(fieldElement, width.closest('.field'))
    width.closest('.field').classList.add('capability-hidden')
    height.closest('.field').classList.add('capability-hidden')
    resolution = fieldElement.querySelector('#video-resolution')
  }
  return resolution
}

function ensureFrameRateControl() {
  const input = document.querySelector('#video-fps')
  if (!input) return null
  if (input.tagName === 'SELECT') return input
  const selectElement = document.createElement('select')
  selectElement.id = 'video-fps'
  selectElement.className = 'control'
  input.replaceWith(selectElement)
  return selectElement
}

function syncVideoCapabilityControls() {
  syncVideoModeOptions();
  const deviceSelect = document.querySelector('#video-device')
  const formatSelect = document.querySelector('#video-format')
  if (!deviceSelect || !formatSelect) return
  const device = selectedVideoDevice()
  if (!device) return
  const modes = device.modes || []
  const formats = [...new Set((modes.length ? modes.map(mode => mode.format) : device.formats || []))]
  const currentFormat = formatSelect.value || state.config.video.input_format
  setCapabilityOptions(formatSelect, formats.map(format => [format, videoFormatLabels[format] || format.toUpperCase()]), currentFormat)
  const format = formatSelect.value
  const formatModes = modes.filter(mode => mode.format === format)
  const resolutions = [...new Set(formatModes.map(mode => mode.resolution))]
  const resolutionSelect = ensureResolutionControl()
  if (!resolutionSelect) return
  const width = document.querySelector('#video-width')
  const height = document.querySelector('#video-height')
  const currentResolution = resolutionSelect.value || (width && height && width.value && height.value ? `${width.value}x${height.value}` : '')
  setCapabilityOptions(resolutionSelect, [['', '自动选择'], ...resolutions.map(resolution => [resolution, resolution])], currentResolution)
  const resolution = resolutionSelect.value
  const resolutionModes = formatModes.filter(mode => !resolution || mode.resolution === resolution)
  const frameRates = [...new Set(resolutionModes.map(mode => String(mode.frame_rate)))]
  const fpsSelect = ensureFrameRateControl()
  setCapabilityOptions(fpsSelect, [['', '自动选择'], ...frameRates.map(fps => [fps, `${fps} fps`])], fpsSelect.value || String(state.config.video.fps || ''))
  if (resolution && width && height) {
    const [nextWidth, nextHeight] = resolution.split('x').map(Number)
    if (nextWidth && nextHeight) { width.value = nextWidth; height.value = nextHeight }
  }
  if (!deviceSelect.dataset.capabilityBound) { deviceSelect.addEventListener('change', syncVideoCapabilityControls); deviceSelect.dataset.capabilityBound = '1' }
  if (!formatSelect.dataset.capabilityBound) { formatSelect.addEventListener('change', syncVideoCapabilityControls); formatSelect.dataset.capabilityBound = '1' }
  if (!resolutionSelect.dataset.capabilityBound) { resolutionSelect.addEventListener('change', syncVideoCapabilityControls); resolutionSelect.dataset.capabilityBound = '1' }
}

async function save() {
  const page=state.page, revision=state.editRevision
  const c=structuredClone(state.config), v=c.video, a=c.audio, r=c.recording, streaming=c.streaming
  const has=id=>!!document.getElementById(id), value=id=>document.getElementById(id)?.value, bool=id=>document.getElementById(id)?.checked

  if (state.page === 'encoding') {
    const device = value('video-device-main') || null
    const mode = (state.videos.find(item => item.path === device) || (!device ? state.videos[0] : null))?.modes?.find(mode => videoModeKey(mode) === value('video-mode-main'))
    if (!mode && device !== v.device) return showMessage('所选视频设备没有可用的输入预设')
    if (mode) { const [width,height] = mode.resolution.split('x').map(Number); Object.assign(v,{ device, width, height, fps:mode.frame_rate, input_format:mode.format }) }
    if (value('video-encoder-main') && value('video-codec-main')) { const software=value('video-encoder-main')==='software'; Object.assign(v,{ encoder:value('video-encoder-main'), codec:value('video-codec-main'), bitrate_kbps:Number(value('video-bitrate-main')), profile:software ? value('video-profile-main') || null : null, level:software ? value('video-level-main') || null : null, preset:software ? value('video-preset-main') || null : null }) }
    a.enabled = bool('audio-enabled-main')
    if (a.enabled) {
      const audioDevice = state.audios.find(item => item.name === value('audio-device-main'))
      const audioMode = audioDevice?.modes?.find(mode => audioModeKey(mode) === value('audio-mode-main'))
      if (!audioDevice || !audioMode) return showMessage('请选择音频设备及其支持的输入预设')
      const channels = audioMode.channels.includes(a.channels) ? a.channels : audioMode.channels[0]
      Object.assign(a,{ device:audioDevice.name, sample_rate:audioMode.sample_rate, input_format:audioMode.format, channels, codec:value('audio-codec-main'), bitrate_kbps:Number(value('audio-bitrate-main')) })
    }
  }

  if(has('recording-directory'))r.directory=value('recording-directory')
  if(has('recording-container'))Object.assign(r,{container:value('recording-container'),segment_seconds:Number(value('recording-segment')),auto_start:bool('recording-auto'),schedule_enabled:bool('recording-schedule'),schedule_start:value('schedule-start'),schedule_stop:value('schedule-stop')})
  if(has('storage-loop'))Object.assign(r,{loop_recording:bool('storage-loop'),min_free_bytes:Number(value('storage-free'))*1048576})
  for (const prefix of ['rtsp','onvif']) {
    if (!has(`${prefix}-enabled`)) continue
    const auth=bool(`${prefix}-auth-enabled`), username=value(`${prefix}-user`)?.trim(), password=value(`${prefix}-pass`)
    if (auth && (!username || !password)) return showMessage('请输入客户端认证用户名和密码')
    Object.assign(streaming[prefix],{enabled:bool(`${prefix}-enabled`),port:Number(value(`${prefix}-port`)),username:auth ? username : null,password:auth ? password : null})
    if (prefix==='rtsp') streaming.rtsp.path=value('rtsp-path').trim()
    else { streaming.onvif.name=value('onvif-name').trim(); if (!streaming.onvif.name) return showMessage('请输入设备名称'); if (streaming.onvif.enabled && !streaming.rtsp.enabled) return showMessage('启用 ONVIF 前请先开启 RTSP 服务') }
  }
  if(has('rtmp-enabled'))Object.assign(streaming.rtmp,{enabled:bool('rtmp-enabled'),url:value('rtmp-url') || null,reconnect_seconds:Number(value('rtmp-reconnect'))})
  if(has('security-user'))Object.assign(c.security,{enabled:true,username:value('security-user'),password:value('security-pass')})
  const securityChanged=has('security-user')
  try {
    const saved=await api('/config',{method:'PUT',body:JSON.stringify(c)})
    if (securityChanged) { sessionStorage.setItem('streambox-auth',encodeCredentials(c.security.username,c.security.password)); sessionStorage.setItem('streambox-user',c.security.username) }
    state.config=saved; state.configEpoch++
    if (state.page===page && state.editRevision===revision) { state.dirty=false; state.message='配置已保存'; render() }
    else showMessage('配置已保存')
    await load(false)
  } catch(error) { showMessage(error.message) }
}


function capture() { const v = state.config.video || {}; const a = state.config.audio || {}; return `<section class="panel"><div class="panel-head"><div><h2>视频输入与编码</h2><p>设备、分辨率、帧率、码率和编码器配置</p></div></div><div class="form-grid">${field('采集设备', select('video-device', v.device, [['', '自动选择'], ...state.videos.map(x => [x.path, `${x.path} · ${x.name}`])]))}${field('输入格式', select('video-format', v.input_format, [['mjpeg', 'MJPEG'], ['yuyv422', 'YUYV 4:2:2'], ['h264', 'H.264']]))}${field('输出编码', select('video-codec', v.codec, [['h264', 'H.264 / AVC'], ['h265', 'H.265 / HEVC']]))}${field('编码器', select('video-encoder', v.encoder, [['auto', '自动选择'], ['hardware', '硬件编码'], ['software', '软件编码']]))}${field('宽度', control('video-width', v.width || 1920, 'number'))}${field('高度', control('video-height', v.height || 1080, 'number'))}${field('帧率', control('video-fps', v.fps || 30, 'number'))}${field('码率（kbps）', control('video-bitrate', v.bitrate_kbps || 4000, 'number'))}${field('Profile', select('video-profile', v.profile || '', [['', '自动'], ['baseline', 'Baseline'], ['main', 'Main'], ['high', 'High']]))}${field('Level', select('video-level', v.level || '', [['', '自动'], ['3.1', '3.1'], ['4.0', '4.0'], ['4.1', '4.1'], ['5.0', '5.0']]))}</div></section><section class="panel"><div class="panel-head"><div><h2>音频输入</h2><p>ALSA 设备、编码格式、采样率、声道和音量</p></div></div><div class="form-grid">${field('启用音频', `<label class="check"><input id="audio-enabled" type="checkbox" ${checked(a.enabled)}> 采集音频</label>`)}${field('ALSA 设备', select('audio-device', a.device, [['', '不选择'], ...state.audios.map(x => [x.name, `${x.name} · ${x.description}${x.available === false ? ' · 不可用' : ''}`])]))}${field('编码格式', select('audio-codec', a.codec || 'aac', [['aac', 'AAC'], ['g711_alaw', 'G.711 A-law / PCMA'], ['g711_ulaw', 'G.711 μ-law / PCMU']]))}${field('采样率', control('audio-rate', a.sample_rate || 48000, 'number'))}${field('声道数', control('audio-channels', a.channels || 2, 'number'))}${field('音量', control('audio-volume', a.volume ?? 100, 'number'))}</div></section><div class="right">${actionButton('save', '保存采集配置', 'primary')}</div>` }

render(); load(); setInterval(()=>load(false), 5000); setInterval(loadPreview, 5000)
