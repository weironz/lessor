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

export const discover = (addr, prefix, sweep = true) =>
  json('/api/discover', {
    method: 'POST',
    body: JSON.stringify({ addr, prefix, sweep }),
  })

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
