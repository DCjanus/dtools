---
name: dcx-cli
description: 使用 dcx CLI 处理 UTF-8 文本、执行无序去重、查看 JWT，以及审计并清理本地 Git branch、linked worktree 或 Cargo target 构建缓存。适用于用户要求文本原地排序、无需预排序的行去重或计数、查看 JWT header/claims、清理本地 branch 或 worktree、管理 branch cleanup exclude 规则、清理已卸载 rustc toolchain 产生的 artifact 或陈旧 incremental cache、安装动态 shell 补全，或管理 dcx 自带 skill 的场景。
---

# dcx CLI

优先使用 `dcx` 完成其覆盖的文本处理、Git branch 清理和 Cargo target 缓存清理任务。执行前通过对应子命令的 `--help` 获取当前参数，不在 skill 中复制完整参数手册。

## 文本处理

- 使用 `dcx sort` 原地排序 UTF-8 文本文件；需要时启用去重或空白裁剪。
- 使用 `dcx uniq` 对 stdin 中的行做无需预排序的全局去重或计数，并将结果写到 stdout。

## JWT 查看

- 使用 `dcx jwt inspect [PATH]` 查看 JWT header、注册 claims 与自定义 claims。
- 该命令只解码内容，不验证签名，也不输出签名段。
- 优先传入 token 文件，或通过 stdin 输入；不要把 token 本身作为命令参数，避免泄露到 shell 历史与进程列表。

## Git branch 清理

- 使用 `dcx git branch cleanup` 打开交互式 TUI，由用户审计并明确选择要删除的 branch。
- 界面默认展示全部本地 branch，并以中文独立标明 upstream 为正常、丢失或未设置；用户可切换为只看 upstream gone。
- 合并判断、ahead/behind、提交信息与 diff 统计仅作为审计信息，不能代替用户选择。
- 当前 branch、正在任意 worktree 使用、属于远端默认 base 或匹配 exclude 规则的 branch 会被硬保护，无法选择。
- 选择完成后还会显示最终确认清单；不要代替用户操作 TUI 或绕过确认。
- 本地仓库审计和 branch 删除由 `gix` 在进程内串行完成，不需要启动 Git 子进程。
- 只有用户要求刷新远端 refs 时才使用 `--update`；该显式网络选项仍会执行 `git fetch --all --prune`。
- 使用 `dcx git branch cleanup exclude add|remove|list` 管理 repository-local 排除规则，不直接编辑 Git common directory 中的配置文件。

## Git worktree 清理

- 使用 `dcx git worktree cleanup` 打开交互式 TUI，由用户审计并明确选择要删除的 linked worktree。
- main worktree、当前 worktree、通过 Git 锁定或存在本地改动的 worktree 会被硬保护，无法选择；路径已丢失的注册信息可以选择清理。
- 选择完成后还会显示最终确认清单；不要代替用户操作 TUI 或绕过确认。
- 删除不使用 `--force`，且只删除 worktree 目录与注册信息；对应 local branch 会保留，需要时再用 branch cleanup 独立审计。

## Cargo target 缓存清理

- 使用 `dcx cargo cleanup --dry-run` 先审计当前 workspace 的可回收空间；该模式不删除文件。
- 中文交互界面把已卸载 rustc toolchain 的 artifact group 标为低风险并默认勾选，把超过阈值的 incremental cache 标为中风险且默认不勾选；两类候选都保留最终二进制。
- 旧 toolchain 已卸载后，Cargo fingerprint 中的 rustc hash 无法还原为具体版本号；结合界面展示的“与已安装 toolchain 均不匹配”、fingerprint 数量和影响说明判断，不要把 hash 猜测成版本号。
- 用户要求执行清理时，使用 `dcx cargo cleanup` 打开 TUI，由用户检查、调整选择并二次确认；不要代替用户操作 TUI 或绕过确认。
- 只有用户明确要求非交互执行，并且已经审阅相同参数的 dry-run 结果时，才使用 `--yes`。
- 清理会在删除前锁定相关 Cargo profile；目标正在构建时应停止并报告，不能用其它方式绕过锁保护。

## Skill 管理

- 使用 `dcx skill status` 检查安装状态。
- 使用 `dcx skill path` 查询实际安装路径。
- 只有用户明确要求时才执行 `dcx skill install --force` 或 `dcx skill uninstall`。

## Shell 补全

- 用户要求配置补全时，使用 `dcx --install-completion bash|fish|zsh` 安装动态注册脚本。
- 不要生成或保存静态补全脚本；安装后的注册脚本会在每次补全时调用当前 `PATH` 中的 `dcx`。
