# Superpowers 技能套件小白上手指南

这是一份给零基础用户的实战版说明书。目标只有一个：

- 让你在 10 分钟内理解这套 skills 为什么强
- 让你当天就能把“会聊天的模型”升级成“会持续交付的工程代理”

---

## 1. 一句话理解：skills 到底是什么

把 skills 理解成“给 AI 装上的作业流程插件”。

没有 skills 时，AI 常见问题是：

- 回答看起来对，但不落地
- 任务做到一半就停，需要你不断催“继续”
- 声称改好了，实际上文件没变

有 skills 时，AI 的行为会更像一个工程团队：

- 先澄清需求（不是立刻乱写）
- 再出方案和计划
- 然后按步骤实现
- 最后强制验证，避免“口头完成”

这就是它强大的地方：**把随机发挥，变成可复现流程**。

---

## 2. 小白最该先记住的 4 个核心技能

如果你现在只想先上手，先用这 4 个就够了：

1. `brainstorming`
作用：把模糊想法变成清晰方案，避免“写到一半推翻重来”。

2. `writing-plans`
作用：把方案拆成可执行任务，明确顺序、边界、验收标准。

3. `executing-plans`
作用：按计划逐项实现，不乱跳步骤。

4. `verification-before-completion`
作用：在声称“完成”前，强制跑验证命令，防止幻觉式完成。

你会发现：只靠这 4 个，稳定性已经比裸跑高很多。

---

## 3. 5 分钟快速起飞（可直接照做）

### 第一步：确认 skills 可用

```bash
zeroclaw skills list
```

如果要安装本地 skill 包：

```bash
zeroclaw skills audit /path/to/skill-or-pack
zeroclaw skills install /path/to/skill-or-pack
zeroclaw skills list
```

说明：`skills install` 前有安全审计，这是好事，能拦危险脚本和不安全链接。

### 第二步：给 AI 下达“流程化请求”

直接复制这句：

```text
帮我用 codex 实现 XXX。先用 $brainstorming 澄清需求和方案，再用 $writing-plans 拆任务，最后用 $executing-plans 执行，并在完成前走 $verification-before-completion。
```

### 第三步：你只盯 3 个结果

- 有没有明确方案（不是空话）
- 有没有可执行计划（不是一坨描述）
- 有没有验证证据（不是“我觉得好了”）

如果三项都齐了，质量通常不会差。

---

## 4. 为什么这套 skills 对小白特别友好

### 好处 A：把“不会提需求”变成“可选题”

很多技能会引导你做 A/B/C 选择。你不需要懂架构，只要能选偏好就行。

### 好处 B：把“怕改坏”变成“可回滚”

计划化执行天然鼓励小步提交、可回滚改动，不会一把梭哈。

### 好处 C：把“玄学调参”变成“证据驱动”

用验证技能后，所有“完成”都要求证据：测试、命令输出、文件变化。

---

## 5. 常用技能地图（按场景选）

| 场景 | 优先技能 | 价值 |
|---|---|---|
| 需求模糊，不知道怎么做 | `brainstorming` | 先把方向做对 |
| 任务多、容易乱 | `writing-plans` | 明确步骤和依赖 |
| 开始落地实现 | `executing-plans` | 按计划稳定推进 |
| 代码有 bug/测试红了 | `systematic-debugging` | 避免拍脑袋修 bug |
| 想提高功能可靠性 | `test-driven-development` | 先定义正确性再写实现 |
| 准备收尾交付 | `verification-before-completion` | 避免“口头完成” |
| 要合并分支/提 PR | `finishing-a-development-branch` | 收尾动作规范化 |
| 需要质量把关 | `requesting-code-review` | 主动拉审查减少漏点 |

---

## 6. 最强组合拳（推荐默认流程）

这是我最推荐给新手的完整链路：

1. `$brainstorming`
2. `$writing-plans`
3. `$executing-plans`
4. `$verification-before-completion`
5. `$requesting-code-review`
6. `$finishing-a-development-branch`

这套链路的核心是：**先对齐，再执行，最后拿证据收口**。

---

## 7. 直接可复制的话术模板

### 模板 1：做新功能

```text
帮我用 codex 实现【功能名】。
要求：
1) 先用 $brainstorming 输出 2-3 个方案并推荐一个；
2) 用 $writing-plans 生成可执行计划；
3) 用 $executing-plans 实现；
4) 完成前必须走 $verification-before-completion，给出验证结果。
```

### 模板 2：修 bug

```text
帮我修这个问题：【贴报错/复现步骤】。
先用 $systematic-debugging 定位根因，再决定修复方案。
修复后用 $verification-before-completion 给我验证证据。
```

### 模板 3：提 PR 前检查

```text
现在进入收尾。
请用 $requesting-code-review 做一次自检，然后用 $finishing-a-development-branch 给出最稳妥的合并方案。
```

---

## 8. 小白最容易踩的 5 个坑

1. 一上来就“直接写代码”
结果：返工率高。先 brainstorm 再写，通常省一半时间。

2. 计划写得太虚
要有“可执行动作 + 验收标准”，不是“优化一下、调整一下”。

3. 没有“完成定义”
你必须明确：什么叫完成？测试通过？文件存在？命令成功？

4. 只看模型回复，不看证据
一定看工具执行、文件变更、测试结果。

5. 技能用太多太杂
一次任务用 1 条主链路就够：`brainstorming -> planning -> executing -> verification`。

---

## 9. 给新手的执行口诀

你可以只记这 12 个字：

**先对齐，后执行，要证据，可回滚。**

只要坚持这条，skills 的威力会非常明显。

---

## 10. 进阶建议（用熟后再看）

1. 多任务并行时，配合 `using-git-worktrees` 隔离改动，避免互相污染。
2. 大任务可以用 `subagent-driven-development` 分治，但必须保留最终验证关口。
3. 收到 code review 意见时，用 `receiving-code-review` 先做技术判断，不盲改。

---

## 附：你可以立即执行的下一步

```text
请按这篇文档的“最强组合拳”帮我完成一个小任务（例如：新增一个 CLI 子命令），并在每一步告诉我当前产出物是什么。
```

把这句发出去，你就已经开始在用 Superpowers 了。
