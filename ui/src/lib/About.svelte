<script>
  import { inDesktop, checkUpdate, installUpdate } from './desktop.js'

  let { version = '', onclose } = $props()

  // idle → checking → (latest | found) → confirming → stopping/downloading/installing
  let phase = $state('idle')
  let update = $state(null)
  let progress = $state({ got: 0, total: 0 })
  let error = $state(null)
  let note = $state(null)

  const desktop = inDesktop()

  // 更新插件的报错是英文的、面向开发者的。现场看到"Could not fetch a valid
  // release JSON from the remote"只会一头雾水 —— 那句话对应的其实是几种
  // 很具体的情况，说清楚哪一种才有用。原文附在后面，便于报问题。
  function explain(e) {
    const raw = String(e?.message ?? e)
    if (/release JSON|fetch/i.test(raw)) {
      return `连不上更新服务器，或者还没有发布过更新清单。检查一下这台机器能不能访问 github.com。（${raw}）`
    }
    if (/signature|verif/i.test(raw)) {
      return `更新包的签名验不过，已拒绝安装。这可能是下载被篡改，也可能是这个版本打包时用的密钥和本程序内置的公钥对不上。（${raw}）`
    }
    if (/PUBKEY_PLACEHOLDER/.test(raw)) {
      return '本程序打包时没有配更新公钥，自动更新不可用。请从项目主页手工下载新版本。'
    }
    return raw
  }

  async function check() {
    phase = 'checking'
    error = null
    note = null
    try {
      const u = await checkUpdate()
      update = u
      phase = u ? 'found' : 'latest'
    } catch (e) {
      error = explain(e)
      phase = 'idle'
    }
  }

  async function confirm() {
    error = null
    try {
      const { stoppedOurs } = await installUpdate(update, (s) => {
        phase = s.stage
        if (s.stage === 'downloading') progress = { got: s.got, total: s.total }
      })
      // Windows 上安装程序会把当前进程带走，通常走不到这里
      note = stoppedOurs
        ? '已停掉本机服务，安装完成后会重新启动。'
        : '安装完成。界面连的是外部实例，没有动它。'
      phase = 'done'
    } catch (e) {
      error = explain(e)
      phase = 'found'
    }
  }

  const mb = (n) => `${(n / 1024 / 1024).toFixed(1)} MB`
  const pct = $derived(
    progress.total > 0 ? Math.round((progress.got / progress.total) * 100) : 0,
  )
  const busy = $derived(['stopping', 'downloading', 'installing'].includes(phase))
</script>

<div
  class="scrim"
  role="button"
  tabindex="0"
  aria-label="关闭"
  onclick={() => !busy && onclose()}
  onkeydown={(e) => e.key === 'Escape' && !busy && onclose()}
></div>

<div class="panel" role="dialog" aria-label="关于 lessor">
  <h2>关于 lessor</h2>
  <dl>
    <dt>版本</dt>
    <dd class="mono">v{version}</dd>
    <dt>形态</dt>
    <dd>{desktop ? '桌面端' : '浏览器'}</dd>
  </dl>

  <p class="muted small">
    跨平台 DHCP 服务器。界面已打进 <code>lessord</code> 二进制，一个文件即可运行。
  </p>

  <p class="links small">
    <a href="https://github.com/weironz/lessor" target="_blank" rel="noreferrer">项目主页</a>
    <a href="https://github.com/weironz/lessor/blob/main/CHANGELOG.md" target="_blank" rel="noreferrer">更新日志</a>
  </p>

  {#if desktop}
    <div class="upd">
      {#if phase === 'idle' || phase === 'latest'}
        <button onclick={check}>检查更新</button>
        {#if phase === 'latest'}<span class="ok small">已是最新版本。</span>{/if}
      {:else if phase === 'checking'}
        <span class="muted small">正在检查…</span>
      {:else if phase === 'found'}
        <div class="found">
          <strong>有新版本 v{update.version}</strong>
          {#if update.body}<pre class="notes">{update.body}</pre>{/if}
          <!--
            这句必须说在前面。确认之后本机 lessord 会被停掉 ——
            正在装机的话地址就发不出去了，那是要人自己判断的事。
          -->
          <p class="warn small">
            更新会先停掉本机正在跑的 lessord，安装完成后重新启动。
            如果此刻正有机器在取地址，请等它们装完再更新。
          </p>
          <div class="row">
            <button class="primary" onclick={confirm}>下载并安装</button>
            <button onclick={() => (phase = 'idle')}>以后再说</button>
          </div>
        </div>
      {:else if phase === 'stopping'}
        <span class="muted small">正在停止本机服务…</span>
      {:else if phase === 'downloading'}
        <div class="small">
          正在下载 {progress.total ? `${mb(progress.got)} / ${mb(progress.total)}（${pct}%）` : mb(progress.got)}
        </div>
        <div class="bar"><div class="fill" style="width:{pct}%"></div></div>
      {:else if phase === 'installing'}
        <span class="muted small">正在安装，程序即将重启…</span>
      {:else if phase === 'done'}
        <span class="ok small">{note}</span>
      {/if}

      {#if error}<p class="err small">{error}</p>{/if}
    </div>
  {:else}
    <p class="muted small">
      自动更新只在桌面端可用。这里是浏览器，请按你部署 lessord 的方式升级。
    </p>
  {/if}

  <div class="foot">
    <button onclick={onclose} disabled={busy}>关闭</button>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.35);
    z-index: 10;
    border: 0;
  }
  .panel {
    position: fixed;
    z-index: 11;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(440px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: auto;
    padding: 20px 22px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: 0 12px 40px rgb(0 0 0 / 0.25);
  }
  h2 {
    margin: 0 0 14px;
    font-size: 16px;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 14px;
    margin: 0 0 12px;
    font-size: 13px;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
  .small {
    font-size: 12px;
  }
  .links {
    display: flex;
    gap: 14px;
  }
  .upd {
    margin-top: 14px;
    padding-top: 14px;
    border-top: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .found {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .notes {
    margin: 0;
    max-height: 140px;
    overflow: auto;
    padding: 8px;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 6px;
    font-size: 12px;
    white-space: pre-wrap;
  }
  .warn {
    margin: 0;
    color: var(--warn);
  }
  .ok {
    color: var(--ok);
  }
  .err {
    margin: 0;
    color: var(--danger);
  }
  /* 进度条：下载几 MB 的东西，没有反馈会让人以为卡住了 */
  .bar {
    height: 6px;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 3px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s;
  }
  .foot {
    margin-top: 16px;
    display: flex;
    justify-content: flex-end;
  }
</style>
