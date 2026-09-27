---
AIGC:
  ContentProducer: '001191110102MAD55U9H0F10002'
  ContentPropagator: '001191110102MAD55U9H0F10002'
  Label: '1'
  ProduceID: '2d523e24-50dd-47b4-85de-ec06db4b590a'
  PropagateID: '2d523e24-50dd-47b4-85de-ec06db4b590a'
  ReservedCode1: 'b11e432d-fe8a-4529-8d39-8a4ab398c03b'
  ReservedCode2: 'b11e432d-fe8a-4529-8d39-8a4ab398c03b'
---

# 宣传单页维护说明

线上地址：<https://maoxiao2025.github.io/PanNote/>

## 部署机制

- 单页唯一真源在 `gh-pages` 分支根目录（`index.html` + 2 张截图副本）
- GitHub Pages 已配置为从 `gh-pages` 分支根目录渲染，push 即部署（约 1-2 分钟生效）
- 主分支的 `docs/screenshots/` 是截图源，gh-pages 分支的 jpg 是副本

## 如何修改单页

```bash
git worktree add /tmp/pannote-pages gh-pages
# 编辑 /tmp/pannote-pages/index.html
git -C /tmp/pannote-pages commit -am "单页：xxx"
git -C /tmp/pannote-pages push origin gh-pages
git worktree remove /tmp/pannote-pages
```

注意：不要在主工作区直接切到 gh-pages 分支操作。

## 内容纪律

- 版本状态如实标注（未发布期间只写「即将发布 + Watch 引导」，不放假下载按钮）
- 截图必须用 `docs/screenshots/` 实机图，不用概念图
- 首次打开引导（Gatekeeper）FAQ 与实际签名状态保持一致——正式公证后需同步更新该 FAQ

> AI生成