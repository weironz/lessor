<script>
  // 静态保留：把某个客户端钉死到固定地址。
  // 现场用它把 BMC 钉到规划地址 —— 比登进 BMC 网页改静态更快，
  // 而且重装、换网卡之外的重启都不会漂。
  import { addReservation, deleteReservation } from './api.js'

  let { scope, onchange } = $props()

  let client = $state('')
  let ip = $state('')
  let hostname = $state('')
  let busy = $state(false)
  let err = $state(null)

  // 界面拿不到保留的明细（/api/state 只给条数），所以这里只做增删，
  // 已有条数从卡片上看。列出明细要等 API 补 GET，属于下一步。
  async function add(e) {
    e.preventDefault()
    busy = true
    err = null
    try {
      await addReservation(scope.id, { client: client.trim(), ip: ip.trim(), hostname: hostname.trim() })
      client = ''
      ip = ''
      hostname = ''
      onchange?.()
    } catch (e2) {
      err = e2.message
    } finally {
      busy = false
    }
  }

  async function remove() {
    if (!client.trim()) return
    busy = true
    err = null
    try {
      await deleteReservation(scope.id, client.trim())
      client = ''
      onchange?.()
    } catch (e2) {
      err = e2.message
    } finally {
      busy = false
    }
  }
</script>

<form onsubmit={add}>
  <p class="cnt">已有 {scope.reservations} 条保留</p>
  <input bind:value={client} spellcheck="false" placeholder="MAC，如 ac:1f:6b:8e:00:99" aria-label="客户端" />
  <div class="row">
    <input bind:value={ip} spellcheck="false" placeholder="固定地址" aria-label="固定地址" />
    <input bind:value={hostname} spellcheck="false" placeholder="主机名（选填）" aria-label="主机名" />
  </div>
  {#if err}<p class="err">{err}</p>{/if}
  <div class="row">
    <button type="submit" disabled={busy || !client.trim() || !ip.trim()}>添加</button>
    <button type="button" class="ghost" disabled={busy || !client.trim()} onclick={remove}>
      按 MAC 删除
    </button>
  </div>
  <p class="hint">
    发 DUID 而非 MAC 的客户端（如 systemd-networkd）用 <code>opt61:</code> 前缀加十六进制。
  </p>
</form>

<style>
  form {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px dashed var(--line);
  }
  .cnt { margin: 0 0 10px; font-size: 12.5px; color: var(--muted); }
  input {
    width: 100%;
    padding: 6px 9px;
    margin-bottom: 8px;
    font: inherit;
    font-size: 13px;
    color: var(--ink);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 4px;
  }
  .row { display: flex; gap: 8px; }
  .row input { margin-bottom: 8px; }
  button {
    font: inherit;
    font-size: 12.5px;
    padding: 6px 12px;
    color: #fff;
    background: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 4px;
    cursor: pointer;
  }
  button.ghost { color: var(--muted); background: none; border-color: var(--line); }
  button:disabled { opacity: .5; cursor: default; }
  .hint { margin: 8px 0 0; font-size: 11.5px; color: var(--muted); }
  .hint code { font-size: 11px; }
  .err { margin: 0 0 8px; font-size: 12.5px; color: #b3452f; }
</style>
