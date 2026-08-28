<script>
  import { patchScope, deleteScope } from './api.js'
  import Reservations from './Reservations.svelte'

  let { scopes = [], listeners = [], onchange } = $props()

  let busy = $state(null)
  let err = $state(null)
  let confirming = $state(null)
  let managing = $state(null)

  const pct = (s) => (s.capacity === 0 ? 0 : Math.round((s.used / s.capacity) * 100))

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
</script>

{#if err}<p class="err">{err}</p>{/if}

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
          <dd class="mono">{s.serverIp ?? '—'}</dd>
          <dt>静态保留</dt>
          <dd class="mono">{s.reservations}</dd>
        </dl>

        <div class="usage">
          <div class="bar" role="img"
               aria-label="已用 {s.used} / 共 {s.capacity}">
            <div class="fill" style="width:{pct(s)}%"></div>
          </div>
          <span class="mono">{s.used} / {s.capacity}</span>
        </div>

        <div class="acts">
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
          <tr><th>本机地址</th><th>绑定网卡</th></tr>
        </thead>
        <tbody>
          {#each listeners as l (l.serverIp)}
            <tr>
              <td class="mono">{l.serverIp}</td>
              <td class="mono muted">{l.iface ?? '未绑定'}</td>
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
</style>
