// 后端接口。开发时由 Vite 转发到本地的 lessord，
// 打包后前端由 lessord 自己提供，同源，不需要配置地址。

async function json(path, init) {
  const res = await fetch(path, {
    headers: { 'content-type': 'application/json' },
    ...init,
  })
  if (!res.ok) {
    let detail = ''
    try {
      const body = await res.json()
      detail = body.error ?? ''
    } catch {
      // 不是 JSON 就算了，状态码本身已经能说明问题
    }
    throw new Error(detail || `${res.status} ${res.statusText}`)
  }
  return res.status === 204 ? null : res.json()
}

export const getState = () => json('/api/state')
export const getLeases = () => json('/api/leases')
export const getInterfaces = () => json('/api/interfaces')

export const revokeLease = (scopeId, ip) =>
  json(`/api/leases/${scopeId}/${ip}`, { method: 'DELETE' })

export const createScope = (body) =>
  json('/api/scopes', { method: 'POST', body: JSON.stringify(body) })

export const patchScope = (id, body) =>
  json(`/api/scopes/${id}`, { method: 'PATCH', body: JSON.stringify(body) })

export const deleteScope = (id) => json(`/api/scopes/${id}`, { method: 'DELETE' })

export const addReservation = (id, body) =>
  json(`/api/scopes/${id}/reservations`, { method: 'POST', body: JSON.stringify(body) })

export const deleteReservation = (id, client) =>
  json(`/api/scopes/${id}/reservations/${encodeURIComponent(client)}`, { method: 'DELETE' })

export const discover = (addr, prefix, sweep = true) =>
  json('/api/discover', {
    method: 'POST',
    body: JSON.stringify({ addr, prefix, sweep }),
  })

// 作用域发现不接受浏览器传入的本机地址：服务端从作用域绑定的监听器取得
// 目标，避免“DHCP 在 A 网卡、扫描却跑到 B 网卡”的错配。
export const discoverScope = (id, { sweep = true, waitMs = 1000 } = {}) =>
  json(`/api/v1/scopes/${id}/discover`, {
    method: 'POST',
    body: JSON.stringify({ sweep, waitMs }),
  })

export const getScopeDevices = (id, { kind } = {}) => {
  const query = kind ? `?kind=${encodeURIComponent(kind)}` : ''
  return json(`/api/v1/scopes/${id}/devices${query}`)
}

// 聚合读取所有已扫描作用域的缓存；这是纯 GET，不会触发任何网络探测。
export const getDevices = ({ kind, scopeId } = {}) => {
  const query = new URLSearchParams()
  if (kind) query.set('kind', kind)
  if (scopeId !== undefined && scopeId !== null) query.set('scopeId', scopeId)
  const suffix = query.size ? `?${query}` : ''
  return json(`/api/v1/devices${suffix}`)
}

/// 连事件流。断开后自动重连 —— 服务重启不该让界面变成死的。
export function connectEvents({ onEvent, onStatus }) {
  let ws = null
  let timer = null
  let closed = false

  const open = () => {
    if (closed) return
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
    ws = new WebSocket(`${proto}//${location.host}/api/events`)

    ws.onopen = () => onStatus('online')
    ws.onmessage = (e) => {
      try {
        onEvent(JSON.parse(e.data))
      } catch {
        // 收到非 JSON 就跳过，不该让整个连接挂掉
      }
    }
    ws.onclose = () => {
      onStatus('offline')
      if (!closed) timer = setTimeout(open, 2000)
    }
    ws.onerror = () => ws?.close()
  }

  open()
  return () => {
    closed = true
    clearTimeout(timer)
    ws?.close()
  }
}

export function fmtDuration(secs) {
  if (secs < 0) return '已过期'
  if (secs < 60) return `${secs} 秒`
  if (secs < 3600) return `${Math.floor(secs / 60)} 分`
  if (secs < 86400) return `${Math.floor(secs / 3600)} 时`
  return `${Math.floor(secs / 86400)} 天`
}

export function fmtClock(unix) {
  return new Date(unix * 1000).toLocaleTimeString('zh-CN', { hour12: false })
}
