# 安装、LLVM 探测与版本回滚

Loom 0.1.0 的发布包同时包含 `loomc`、`loom-lsp` 与该平台的 `runtime/` bundle。每个平台 archive 旁都有同名
`.sha256` 文件；安装前必须用 `shasum -a 256 -c`（Linux 可用 `sha256sum -c` 转换后的
清单）验证。当前 release workflow 生成 Linux x86-64 与 macOS arm64 两种包，不把某一
宿主的 runtime archive 冒充为其他 target 的 runtime。

`runtime/loom-runtime-bundle.json` 固定目标 triple、LLVM data layout、runtime ABI、archive
SHA-256 与必要 system link args。`loomc runtime export --output DIR` 可从本机同版本工具重新
导出完全相同语义边界的宿主 bundle；目标目录必须不存在。交叉 executable 构建必须显式成对
传入 `--runtime-bundle DIR --linker PROGRAM`。推荐直接使用对应目标发布包中的 `runtime/`，并
使用真正支持该 target 的 linker；编译器会在链接前校验 bundle target/ABI/digest，并把 bundle
与 linker identity 纳入缓存。仅需要 relocatable object 时不需要 runtime bundle。

当前 native compatibility Value ABI 只支持 64-bit pointer target；32-bit triple 在产生 object
前以 `UnsupportedNativePointerWidth` fail closed。当前 runtime identity 包含
`loom-value-v2/layout-v1/text-v1/gc-v2/stdlib-v3`，旧 runtime bundle 会因 ABI 不匹配而拒绝。

## LLVM 19

编译 Loom 工具链需要 LLVM 19 开发文件。Ubuntu 24.04 使用
`clang-19 llvm-19-dev libpolly-19-dev`，并设置
`LLVM_SYS_191_PREFIX=/usr/lib/llvm-19`、`LOOM_CC=clang-19`。macOS 使用
`brew install llvm@19`，然后设置：

```sh
export LLVM_SYS_191_PREFIX="$(brew --prefix llvm@19)"
export LOOM_CC="$LLVM_SYS_191_PREFIX/bin/clang"
export PATH="$LLVM_SYS_191_PREFIX/bin:$PATH"
llvm-config --version
```

`llvm-config --version` 必须报告 `19.x`；找不到 LLVM、版本不匹配或目标机器信息无法建立
都会让构建失败，不会静默退回另一 LLVM。Loom 当前的语言标准库是编译器随版本固定的
`loom-core-inline-v2`，没有环境搜索路径，也不会从当前目录加载同名模块。package source
只来自 resolved manifest/path/registry/validated `.loomlib` graph。

## 兼容与回滚

manifest 的 `language = "0.3"`、`.loomlib` envelope、checked-MIR envelope、runtime ABI
和 cache schema 分别版本化。新工具拒绝未知语言或 artifact 版本；cache 不兼容只产生 miss。
回滚时下载上一个已归档版本及其 SHA-256，验证后原子替换 `loomc`/`loom-lsp`。不要只替换
其中一个二进制，也不要把新版本生成的 cache 目录复制给旧版本。项目源码与 `loom.lock`
应保留在版本控制中；若旧编译器报告版本不兼容，应恢复由该编译器生成的 artifact，而不是
手工改写 envelope/version 字段。
