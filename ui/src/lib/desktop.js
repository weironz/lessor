// 桌面壳专属能力。
//
// 同一份界面有两个去处：浏览器里打开，或者装在桌面壳里。自动更新只在后者
// 有意义 —— 浏览器里"更新"这件事没有对应物。所以这里的每个入口都先问一句
// 在不在壳里，不在就当作没有这个功能，而不是报错。
//
// Tauri 的 JS 包用动态 import：浏览器里那条路径压根不会去加载它们，
// 也就不会因为缺少 __TAURI_INTERNALS__ 而炸掉。

/** 现在是不是跑在桌面壳里。 */
export const inDesktop = () =>
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

/**
 * 问一下有没有新版本。
 *
 * 返回 `null` 表示已经是最新，或者根本不在桌面壳里。
 */
export async function checkUpdate() {
  if (!inDesktop()) return null
  const { check } = await import('@tauri-apps/plugin-updater')
  return check()
}

/**
 * 下载并安装。
 *
 * 装之前**必须先停掉本机那个 lessord** —— Windows 上安装程序要覆盖
 * lessord.exe，而它此刻正在跑，文件被占用就装不上去。停不掉的话装出来的
 * 会是"新外壳 + 旧服务"，比装失败更难查。
 *
 * `onStage` 会被依次调用，用来给界面报进度。
 * 返回一句话说明本机服务是被我们停掉的、还是本来就是外部实例。
 */
export async function installUpdate(update, onStage) {
  const { invoke } = await import('@tauri-apps/api/core')

  onStage?.({ stage: 'stopping' })
  // 只停我们自己拉起的那个。attach 上去的外部实例（系统服务、别人手工起的）
  // 不归这里管 —— 悄悄杀掉别人的 DHCP 服务器，后果比更新失败严重得多。
  const stoppedOurs = await invoke('stop_local_server')

  let total = 0
  let got = 0
  await update.downloadAndInstall((e) => {
    if (e.event === 'Started') {
      total = e.data.contentLength ?? 0
      onStage?.({ stage: 'downloading', got: 0, total })
    } else if (e.event === 'Progress') {
      got += e.data.chunkLength ?? 0
      onStage?.({ stage: 'downloading', got, total })
    } else if (e.event === 'Finished') {
      onStage?.({ stage: 'installing' })
    }
  })

  // Windows 上安装程序会自己把当前进程带走，走不到这里；
  // 其他平台需要我们自己重启。
  try {
    const { relaunch } = await import('@tauri-apps/plugin-process')
    await relaunch()
  } catch {
    // 已经被安装程序接管了，正常
  }
  return { stoppedOurs }
}
