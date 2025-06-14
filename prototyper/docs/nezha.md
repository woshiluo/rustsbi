# 哪吒测试指南

- https://github.com/woshiluo/SyterKit
- https://github.com/woshiluo/rustsbi

## SD 卡

应当保证如下图所示，分区大小可以随意：

```plain
Disk /dev/sda: 119.08 GiB, 127865454592 bytes, 249737216 sectors
Disk model: Storage Device
Units: sectors of 1 * 512 = 512 bytes
Sector size (logical/physical): 512 bytes / 512 bytes
I/O size (minimum/optimal): 512 bytes / 512 bytes
Disklabel type: dos
Disk identifier: 0x418ddfa6

Device     Boot Start     End Sectors Size Id Type
/dev/sda1        2048 4196351 4194304   2G  c W95 FAT32 (LBA)
```

`/dev/sda1` 应为 fat32 格式，且在根目录下放置 `rustsbi.bin` 文件。

`rustsbi.bin` 应为 prototyper 编译后的 `rustsbi-prototyper-payload.bin` 文件。

## Prototyper

使用仓库：<https://github.com/woshiluo/rustsbi>
分支：`test/d1`

```
cargo prototyper --fdt ./sunxi.dtb --payload ./Image
```

（这两个文件应当通过放在项目根目录下，Image 文件应通过其他渠道获取）

编译后 `target/riscv64imac-unknown-none-elf/release/rustsbi-prototyper-payload.bin` 即为上文所需。

## 刷写 SyterKit

使用仓库：<https://github.com/woshiluo/SyterKit>
分支：`test/d1`

首先，应当安装 xfel 并加入到 PATH。（Archlinux 用户可以直接安装 aur：<https://aur.archlinux.org/packages/xfel>）

将开发板上的 OTG 口连接到电脑，按住 FEL 按钮的同时插入电源线，电源指示灯亮起即可松开按钮。

随后在项目下执行：

```bash
cargo flash --release
```

以刷写 SyterKit。

由于该 xtask 目前不能够正确识别 nand Flash 和 nor Flash，如果遇到刷写错误，请将 `rust/xtask/src/main.rs` 文件的 L219-220 中的 `spinor` 替换成 `spinand`。

## 测试

正常上电。

理想情况下，应当在 UART 输出 RustSBI Prototyper 的欢迎页面。（可能需要等待 ~2min，加载需要时间）

非理想情况下，可能会报错 `Invalid MBR signature` 并 fallback 到 shell 中。
