<script>
  import { fmtClock } from './api.js'

  let { packets = [], onclear } = $props()

  const RESULT_STYLE = {
    OFFER: 'accent',
    ACK: 'ok',
    NAK: 'warn',
    DROP: 'danger',
    HANDLED: '',
  }
</script>

<div class="bar">
  <span class="muted">
    实时显示每个 DHCP 报文的处理结果。最多保留最近 300 条。
  </span>
  <button onclick={onclear} disabled={packets.length === 0}>清空</button>
</div>

{#if packets.length === 0}
  <div class="empty">还没有报文。设备接上网线后这里会实时滚动。</div>
{:else}
  <div class="tablewrap">
    <table>
      <thead>
        <tr>
          <th>时间</th>
          <th>客户端</th>
          <th>请求</th>
          <th>结果</th>
          <th>地址</th>
          <th>说明</th>
        </tr>
      </thead>
      <tbody>
        {#each packets as p, i (`${p.at}-${i}-${p.client}`)}
          <tr>
            <td class="mono muted">{fmtClock(p.at)}</td>
            <!--
              option 60 跟着客户端一起显示，不单开一列：它常常很长
              （PXEClient:Arch:00007:UNDI:003016），单开一列会把别的信息挤掉。
              现场一屏 MAC 谁也认不出哪台是哪台，这行字才是认设备用的。
            -->
            <td class="mono">
              {p.client}
              {#if p.vendorClass}<span class="vc" title={p.vendorClass}>{p.vendorClass}</span>{/if}
            </td>
            <td class="mono">{p.request}</td>
            <td>
              <span class="pill {RESULT_STYLE[p.result] ?? ''}">{p.result}</span>
            </td>
            <td class="mono">{p.ip ?? '—'}</td>
            <td class="muted">{p.detail ?? ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 12px;
    font-size: 13px;
  }
  /* option 60 是辅助信息，不能盖过 MAC。太长的截断，鼠标悬停看全文 */
  .vc {
    display: block;
    max-width: 28ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: 11px;
  }
</style>
