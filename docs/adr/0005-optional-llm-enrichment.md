# ADR-0005: LLM は任意の後段エンリッチとして隔離する

## ステータス

承認済み

## コンテキスト

改善提案はテンプレート生成を基本としつつ、`--llm` 指定時に文脈依存の提案を拡張できることが求められている。一方で、LLM の応答有無に kalos 全体が依存してはならない。

- `REQ-FUNC-015`
- `REQ-NF-008`
- `REQ-NF-003`

## 検討した選択肢

### 選択肢 A: 診断生成の途中で LLM を同期呼出しする

- 利点:
  - 診断生成と提案生成を一体化できる
- 欠点:
  - LLM 障害がコア診断へ波及する
  - 決定論性が崩れやすい

### 選択肢 B: テンプレート提案を決定論的コアで生成し、LLM は後段 sidecar として追記する

- 利点:
  - LLM 障害を隔離できる
  - 診断・重大度・Exit code を不変に保てる
  - `--llm` 未使用時と基本契約が一致する
- 欠点:
  - 提案文が二層になる
  - API キーや送信データ制御が別途必要

### 選択肢 C: LLM 提案を完全非同期の別コマンドに分離する

- 利点:
  - コアから完全に切り離せる
- 欠点:
  - UX が分断される
  - 初回リリースの操作性が落ちる

## 判断

選択肢 B を採用する。

## 根拠

- `REQ-NF-008` の「LLM 非応答でも全体可用性を維持」を満たすには、テンプレート提案を正本とする必要がある
- `REQ-NF-003` を守るため、スコア・重大度・Exit code はテンプレート側だけで確定させる
- LLM への入力は application/report 境界で組み立てた allowlist 済み `LlmEnrichmentRequest` `{ rule_id, severity, language, workspace_relative_path, metric?, pattern?, source_excerpt?, cpg_excerpt? }` に限定する。`language` は `Diagnostic.location.file_path` に対応する代表ファイルのメタデータから解決し、必須根拠を代表ファイル断片へ還元できる場合にだけ request を生成する
- LLM 出力は `DiagnosticId` ごとの `LlmSuggestionBundle` として report 層で併記し、`DiagnosticReport` 自体は変更しない
- LLM は optional sidecar budget（`connect timeout = 3s`, `overall timeout = 30s`, `retry = 0`）で実行し、タイムアウト・非応答・言語解決不能時、または multi-file / multi-language 診断の根拠を代表ファイル断片へ還元できない時は `llm_suggestion` を省略してテンプレート提案のみ返す

## 帰結

### ポジティブ

- LLM 障害が CI 判定やスコアへ影響しない
- 送信ポリシーとタイムアウト戦略をアダプタ層へ閉じ込められる
- テンプレート提案を最低保証として維持できる
- LLM へ送る入力面積を診断単位の最小断片に限定できる

### ネガティブ

- テンプレート提案と sidecar 提案の整形ルールが必要
- `--llm` 時の待ち時間が追加される

### リスク

- LLM が冗長または不正確な提案を返す可能性があるため、テンプレート結果を常に併記し、ユーザーが比較できるようにする
