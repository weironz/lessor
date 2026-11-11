<script>
  import { patchScope, deleteScope } from './api.js'
  import Reservations from './Reservations.svelte'
  import NewScope from './NewScope.svelte'

  let { scopes = [], listeners = [], onchange, oncreated = null } = $props()

  let busy = $state(null)
  let err = $state(null)
  let confirming = $state(null)
  let managing = $state(null)
  let adding = $state(false)
  let editing = $state(null)
  let draft = $state({ name: '', poolStart: '', poolEnd: '', router: '', leaseSecs: '' })

  const pct = (s) => (s.capacity === 0 ? 0 : Math.round((s.used / s.capacity) * 100))

  // 引导配置只读展示。改它要写四个字段（文件名、next-server、HTTP URL、
  // iPXE 脚本），随手一个输入框改不完整，容易改出"地址拿到了机器不动"。
  const bootSummary = (b) => b?.ipxeUrl ?? b?.httpUrl ?? b?.filename ?? '未配置'

  async function run(id, fn) {
    busy = id
    err = null
    try {
      await fn()
      onchange?.()
    } catch (e) {
      err = e.message
    } finally {
      busy = null
      confirming = null
    }
  }

  const toggle = (s) => run(s.id, () => patchScope(s.id, { enabled: !s.enabled }))
  const remove = (s) => run(s.id, () => deleteScope(s.id))

  function beginEdit(s) {
    err = null
    managing = null
    editing = s.id
    draft = {
      name: s.name,
      poolStart: s.poolStart ?? '',
      poolEnd: s.poolEnd ?? '',
      router: s.router ?? '',
      leaseSecs: String(s.leaseSecs ?? ''),
    }
  }

  function saveEdit(s) {
    const name = draft.name.trim()
    const poolStart = draft.poolStart.trim()
    const poolEnd = draft.poolEnd.trim()
    const router = draft.router.trim()
    const leaseSecs = Number(draft.leaseSecs)
    if (!name || !poolStart || !poolEnd || !Number.isInteger(leaseSecs) || leaseSecs < 30) {
      err = '名称、完整地址池和不少于 30 秒的租期为必填项'
      return
    }
    run(s.id, async () => {
      await patchScope(s.id, {
        name,
        poolStart,
        poolEnd,
        // 显式 null 是清空网关；后端用双层 Option 区分它与“不修改”。
        router: router || null,
        leaseSecs,
      })
      editing = null
    })
  }
</script>

{#if err}<p class="err">{err}</p>{/if}

{#if adding}
  <NewScope
    compact
    oncreated={(target) => {
      adding = false
      onchange?.()
      oncreated?.(target)
    }}
    oncancel={() => (adding = false)}
  />
{:else}
  <div class="toolbar">
    <p>切换到其他网卡或网段时，先新增正确的作用域，再停用或删除旧作用域。</p>
    <button class="link" onclick={() => (adding = true)}>新增作用域</button>
  </div>
{/if}

{#if scopes.length === 0}
  <div class="empty">没有配置作用域</div>
{:else}
  <div class="grid">
    {#each scopes as s (s.id)}
      <section class="card">
        <div class="head">
          <h2>{s.name}</h2>
          <button
            class="pill {s.enabled ? 'ok' : 'warn'}"
            disabled={busy === s.id}
            onclick={() => toggle(s)}
            title={s.enabled ? '点击禁用（停止应答，配置保留）' : '点击启用'}
          >
            {s.enabled ? '启用' : '已禁用'}
          </button>
        </div>

        <dl>
          <dt>网段</dt>
          <dd class="mono">{s.subnet}/{s.prefix}</dd>
          <dt>本机地址</dt>
          <!-- 经中继服务的网段本机就没有地址，那是有意的；
               直接显示 '—' 会看起来像配漏了监听器 -->
          <dd class="mono">
            {#if s.viaRelay}经 DHCP 中继{:else}{s.serverIp ?? '—'}{/if}
          </dd>
          <dt>静态保留</dt>
          <dd class="mono">{s.reservations}</dd>
          <dt>地址池</dt>
          <dd class="mono">{s.poolStart && s.poolEnd ? `${s.poolStart} – ${s.poolEnd}` : '仅静态保留'}</dd>
          <dt>网关</dt>
          <dd class="mono">{s.router ?? '—'}</dd>
          <dt>网络引导</dt>
          <dd class="mono boot" title={bootSummary(s.boot)}>{bootSummary(s.boot)}</dd>
        </dl>

				{#if !s.viaRelay && s.listenerPresent === false}
          <p class="stale">
            监听地址已不在本机。请用“新增 / 更换网卡”选择当前网卡，再停用或删除此作用域。
          </p>
        {/if}

        <div class="usage">
          <div class="bar" role="img"
               aria-label="已用 {s.used} / 共 {s.capacity}">
            <div class="fill" style="width:{pct(s)}%"></div>
          </div>
          <span class="mono">{s.used} / {s.capacity}</span>
        </div>

        <div class="acts">
          <button class="link" onclick={() => (editing === s.id ? (editing = null) : beginEdit(s))}>
            {editing === s.id ? '收起编辑' : '编辑'}
          </button>
          <button class="link" onclick={() => (managing = managing === s.id ? null : s.id)}>
            {managing === s.id ? '收起保留' : '静态保留'}
          </button>
          {#if confirming === s.id}
            <span class="confirm">
              连同租约一起删？
              <button class="link danger" disabled={busy === s.id} onclick={() => remove(s)}>删除</button>
              <button class="link" onclick={() => (confirming = null)}>取消</button>
            </span>
          {:else}
            <button class="link danger" onclick={() => (confirming = s.id)}>删除</button>
          {/if}
        </div>

        {#if editing === s.id}
          <form class="editor" onsubmit={(e) => { e.preventDefault(); saveEdit(s) }}>
            <label>名称<input bind:value={draft.name} /></label>
            <div class="edit-row">
              <label>地址池起始<input bind:value={draft.poolStart} spellcheck="false" /></label>
              <label>地址池结束<input bind:value={draft.poolEnd} spellcheck="false" /></label>
            </div>
            <div class="edit-row">
              <label>网关<input bind:value={draft.router} placeholder="可留空" spellcheck="false" /></label>
              <label>租期（秒）<input bind:value={draft.leaseSecs} inputmode="numeric" spellcheck="false" /></label>
            </div>
            {#if s.viaRelay}
              <p class="hint">Relay 网段、前缀和服务方式不可原地修改；它们决定 giaddr 匹配规则，需新建作用域后再停用旧作用域。</p>
            {:else}
              <p class="hint">网段、前缀和监听地址不可原地修改；切换网卡请新建作用域。</p>
            {/if}
            <button type="submit" disabled={busy === s.id}>保存修改</button>
          </form>
        {/if}

        {#if managing === s.id}
          <Reservations scope={s} {onchange} />
        {/if}
      </section>
    {/each}
  </div>

  {#if listeners.length}
    <h3 class="sub">监听器</h3>
    <div class="tablewrap">
      <table>
        <thead>
          <tr><th>本机地址</th><th>所在网卡</th></tr>
        </thead>
        <tbody>
          {#each listeners as l (l.serverIp)}
            <tr>
              <td class="mono">{l.serverIp}</td>
              <td class="mono muted">{l.iface ?? '未找到'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
{/if}

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 14px;
  }
  .card {
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 14px 16px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
  }
  h2 { font-size: 15px; }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px 12px;
    margin: 0 0 12px;
    font-size: 13px;
  }
  dt { color: var(--muted); }
  dd { margin: 0; text-align: right; }
  /* 引导 URL 往往很长，撑开卡片会把整个列表挤变形 */
  dd.boot { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .usage {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
  }
  .bar {
    flex: 1;
    height: 6px;
    background: var(--panel-2);
    border-radius: 20px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
  }
  .acts {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 12px;
    padding-top: 10px;
    border-top: 1px solid var(--line);
    font-size: 12.5px;
  }
  .link {
    font: inherit;
    font-size: 12.5px;
    color: var(--accent);
    background: none;
    border: 0;
    padding: 0;
    cursor: pointer;
  }
  .link:hover { text-decoration: underline; }
  .link.danger { color: #b3452f; }
  .link:disabled { opacity: .5; cursor: default; }
  .confirm { display: flex; align-items: center; gap: 8px; color: var(--muted); }
  .head button.pill {
    font: inherit;
    cursor: pointer;
  }
  .err {
    margin: 0 0 14px;
    padding: 9px 12px;
    font-size: 13px;
    color: #b3452f;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
  }
  .sub {
    margin: 24px 0 10px;
    font-size: 13px;
    color: var(--muted);
  }
  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin: 0 0 14px;
    font-size: 13px;
  }
  .toolbar p { margin: 0; color: var(--muted); }
  .stale {
    margin: -2px 0 12px;
    padding: 8px 10px;
    color: #8a5a00;
    background: var(--warn-bg);
    border: 1px solid var(--line);
    border-radius: 5px;
    font-size: 12.5px;
    line-height: 1.45;
  }
  .editor { margin-top: 12px; padding-top: 12px; border-top: 1px dashed var(--line); }
  .editor label { display: block; margin-bottom: 8px; font-size: 12px; color: var(--muted); }
  .editor input {
    width: 100%; box-sizing: border-box; margin-top: 4px; padding: 6px 8px;
    font: inherit; font-size: 12.5px; color: var(--ink); background: var(--bg);
    border: 1px solid var(--line); border-radius: 4px;
  }
  .edit-row { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
  .editor .hint { margin: 2px 0 10px; font-size: 11.5px; color: var(--muted); line-height: 1.4; }
  .editor button {
    padding: 6px 10px; font: inherit; font-size: 12.5px; color: #fff;
    background: var(--accent); border: 1px solid var(--accent); border-radius: 4px; cursor: pointer;
  }
</style>
