/// 构建期注入的常量。见 `vite.config.ts` 的 `define`。
///
/// 客户端（壳体）自己的版本号，来自 `package.json`（beta 打包时会被流水线戳成
/// 完整 tag）。跟侧栏底部那个内核版本不是一回事：那个是运行时从内核问来的。
declare const __CLIENT_VERSION__: string;
