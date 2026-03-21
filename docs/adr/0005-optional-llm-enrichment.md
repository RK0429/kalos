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
- LLM への入力は Application Pipeline が `Diagnostic` と `SourceAnalysis` から組み立てる allowlist 済み `LlmEnrichmentRequest` `{ rule_id, severity, language, workspace_relative_path, metric?, pattern?, source_excerpt?, cpg_excerpt? }` に限定する。`language` は `Diagnostic.location.file_path` に対応する `SourceAnalysis.source_files` の代表ファイルメタデータから取得する。`source_excerpt` と `cpg_excerpt` は request ごとに相互排他的であり、どちらか一方だけを持つ。`metric` と `pattern` は `Diagnostic.kind` に応じて排他的に設定される。必須根拠を代表ファイル断片へ還元できる場合にだけ request を生成する
- LLM 出力は `DiagnosticId` ごとの `LlmSuggestionBundle` として report 層で併記し、`DiagnosticReport` 自体は変更しない
- **Preflight（request 生成抑止）**: 以下の条件に該当する診断には `LlmEnrichmentRequest` 自体を生成しない。テンプレート提案のみ返す
  - `SourceAnalysis.source_files` から代表ファイルの言語を一意に解決できない場合
  - multi-file / multi-language 診断の必須根拠を代表ファイル断片へ還元できない場合
- **Sidecar budget（per `LlmEnrichmentRequest`）**: `connect timeout = 3s`, `overall timeout = 30s`, `retry = 0`。タイムアウトは個々の `LlmEnrichmentRequest`（= 診断単位の LLM API 呼び出し）ごとに適用する。複数診断がある場合の総所要時間は LLM Adapter の並行度に依存する（実装詳細）
- **Post-dispatch fallback（送信後の障害処理）**: `LlmEnrichmentRequest` の送信後にタイムアウト・非応答・エラーが発生した場合、当該診断の `llm_suggestion` のみを省略し、テンプレート提案を返す。コア診断・スコア・Exit code は不変

## 帰結

### ポジティブ

- LLM 障害が CI 判定やスコアへ影響しない
- 送信ポリシーとタイムアウト戦略をアダプタ層へ閉じ込められる
- テンプレート提案を最低保証として維持できる
- LLM へ送る入力面積を診断単位の最小断片に限定できる

### ネガティブ

- テンプレート提案と sidecar 提案の整形ルールが必要
- `--llm` 時の待ち時間が追加される
- **API キー管理**: 環境変数 `KALOS_LLM_API_KEY` で提供する。kalos は永続化しない
- **Outbound 通信**: `--llm` は LLM エンドポイントへのネットワークアクセスを暗示する（REQ-NF-009）。エンドポイント URL の設定方法（環境変数・設定ファイル・プロバイダ固有のデフォルト等）は LLM Adapter の実装仕様として別途定義する。接続先エンドポイント URL は info レベルでログ出力する（ペイロードは出力しない）
- **データ機密性**: `source_excerpt` / `cpg_excerpt` はプロプライエタリコードを含む可能性がある。`--llm` の指定をもってユーザーの明示的オプトインとする
- **監査境界**: リクエスト/レスポンスのメタデータ（タイムスタンプ、トークン数、ステータスコード）は debug レベルで構造化ログに出力する。コンテンツ自体はログに含めない

### リスク

- LLM が冗長または不正確な提案を返す可能性があるため、テンプレート結果を常に併記し、ユーザーが比較できるようにする
