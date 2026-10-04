# 本地 TXT 阅读器 v1.3.0

这是一个本地 TXT 阅读器，单网页程序可以跨平台运行，只要有浏览器就可以。

![Windows](https://img.shields.io/badge/Windows-0078D6?style=flat&logo=windows&logoColor=white)
![macOS](https://img.shields.io/badge/macOS-000000?style=flat&logo=apple&logoColor=white)
![Linux](https://img.shields.io/badge/Linux-FCC624?style=flat&logo=linux&logoColor=black)
![Android](https://img.shields.io/badge/Android-3DDC84?style=flat&logo=android&logoColor=white)
![iOS](https://img.shields.io/badge/iOS-000000?style=flat&logo=apple&logoColor=white)
![Chrome](https://img.shields.io/badge/Chrome-4285F4?style=flat&logo=googlechrome&logoColor=white)
![Edge](https://img.shields.io/badge/Edge-0078D6?style=flat&logo=microsoftedge&logoColor=white)

## 功能

- 纯前端单文件 HTML：无需安装、无需联网、不上传任何文件
- 打开本地 TXT 即读，自动分页，每页底部不出现半行截断
- 自动识别章节（第X章/回/卷/楔子/尾声等），目录点击跳转
- 字体选择：宋体 / 黑体 / 微软雅黑 / 楷体 / 仿宋 / 等线 / 隶书 / 幼圆 及常见西文字体
- 自动翻页：间隔 0.5s – 3s 可调，每次自动上移一行，空格键启动 / 停止
- 鼠标滚轮翻页：向下滚下一页，向上滚上一页
- 字号、行高、文字色、背景色可调
- 阅读进度按文件保存在浏览器本地（localStorage）
- UTF-8 / GBK 编码手动切换

## 使用

在线版：https://744733778.github.io/txt-reader/

或下载 [index.html](https://github.com/744733778/txt-reader/releases/latest) 双击打开。阅读区点击：左 1/3 上一页 · 中 1/3 设置 · 右 1/3 下一页；键盘 ← / → 翻页。

## 更新日志

- **v1.3.0**：桌面版改为无边框窗口（自绘标题栏、中间 1/3 拖动窗口、右下角缩放柄）；新增老板模式（鼠标离开窗口自动隐藏）；新增启动日志（%TEMP%\txt-reader.log）排查静默启动失败；标题栏显示当前章节名；ESC 关闭菜单 / 退出程序；自动翻页速度改为滑条调节
- **v1.2.0**：新增字体选择、自动翻页（空格启停、逐行上移）、鼠标滚轮翻页
- **v1.1.0**：新增自动章节识别与目录跳转，移除示例文本
- **v1.0.0**：初版，无半行分页、阅读记忆、三分区翻页
