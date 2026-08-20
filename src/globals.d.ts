/// 构建期注入的常量。见 `vite.config.ts` 的 `define`。
///
/// 客户端（壳体）自己的版本号，来自 `package.json`。跟侧栏底部那个内核版本不是
/// 一回事：那个是运行时从内核问来的，这个是打包时钉进来的。
declare const __CLIENT_VERSION__: string;
