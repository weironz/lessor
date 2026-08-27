<script>
  let { scopes = [], listeners = [] } = $props()

  const pct = (s) => (s.capacity === 0 ? 0 : Math.round((s.used / s.capacity) * 100))
</script>

{#if scopes.length === 0}
  <div class="empty">没有配置作用域</div>
{:else}
  <div class="grid">
    {#each scopes as s (s.id)}
      <section class="card">
        <div class="head">
          <h2>{s.name}</h2>
          <span class="pill {s.enabled ? 'ok' : 'warn'}">
            {s.enabled ? '启用' : '已禁用'}
          </span>
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
  .sub {
    margin: 24px 0 10px;
    font-size: 13px;
    color: var(--muted);
  }
</style>
