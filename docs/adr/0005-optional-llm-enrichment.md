# ADR-0005: LLM は任意の後段エンリッチとして隔離する

## メタ情報

| 項目 | 内容 |
|---|---|
| 承認日 | 2026-03-18 |
| 最終更新日 | 2026-03-27 |
| 改訂 | v0.4.7 |

> **注記**: メタ情報の `改訂` は本 ADR 自体の版番号であり、改訂履歴の `関連文書版` 列に記載される architecture.md / requirements.md の版番号とは独立したバージョニング体系である。

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
- **決定論性契約との関係**: `llm_suggestion`（`LlmSuggestionBundle`）は ADR-0003 の決定論性契約の適用範囲外である。LLM 応答の非決定性、wall-clock budget の消費状況、ネットワーク状態により、同一入力・同一設定でも `llm_suggestion` の有無・内容は変動しうる。決定論性契約が保証する出力要素（`ScopeMetrics`、`Diagnostic`、`OverallScore`、`DiagnosticReport`、Exit code、評価順序）は `--llm` の有無にかかわらず不変である
- LLM への入力は Application Pipeline が `Diagnostic` と `SourceAnalysis`（ADR-0002 参照）から組み立てる allowlist 済み `LlmEnrichmentRequest` `{ rule_id, severity, language, workspace_relative_path, metric?, pattern?, source_excerpt?, cpg_excerpt? }` に限定する。`language` は `Diagnostic.location.file_path` に対応する `SourceAnalysis.source_files` の代表ファイルメタデータから取得する。`source_excerpt` と `cpg_excerpt` は request ごとに相互排他的であり、どちらか一方だけを持つ。`metric` と `pattern` は `Diagnostic.kind` に応じて排他的に設定される。必須根拠を代表ファイル断片へ還元できる場合にだけ request を生成する
- LLM 出力は `DiagnosticId` ごとの `LlmSuggestionBundle` として report 層で併記し、`DiagnosticReport` 自体は変更しない
- **Preflight（request 生成抑止）**: 以下の条件に該当する診断には `LlmEnrichmentRequest` 自体を生成しない。テンプレート提案のみ返す
  - `SourceAnalysis.source_files` から代表ファイルの言語を一意に解決できない場合（v1 の対象言語 Python/TypeScript/Rust/Go ではファイル拡張子から言語が一意に確定するため通常は該当しないが、将来の言語追加時に拡張子が複数言語で共有されるケースへの forward compatibility として条件を保持する）
  - multi-file / multi-language 診断の必須根拠を代表ファイル断片へ還元できない場合
- **Preflight failure（request 生成前の障害処理）**: `--llm` が指定されたが `KALOS_LLM_API_KEY` が未設定の場合は設定エラー（exit code 2）とする。`KALOS_LLM_ENDPOINT_URL` が不正な URL 構文の場合も同様とする。`KALOS_LLM_PROVIDER` が v1 の許容値（`openai`）以外の値に設定されている場合も設定エラー（exit code 2）とし、サポートされていないプロバイダ名とサポート済みプロバイダの一覧をエラーメッセージに含める。Preflight 条件（代表ファイルの言語解決不可、multi-file 診断の断片還元不可）に該当する診断は `LlmEnrichmentRequest` を生成せず、テンプレート提案のみ返す。Preflight 条件による request 省略は warning を出さない（正常動作）
- **Sidecar budget（per `LlmEnrichmentRequest`）**: `connect timeout = 3s`, `overall timeout = 30s`。タイムアウトは個々の `LlmEnrichmentRequest`（= 診断単位の LLM API 呼び出し）ごとに適用する。**v1 ディスパッチポリシー**: v1 では LLM Adapter は逐次実行（max in-flight = 1）とし、並行ディスパッチは行わない。HTTP 応答ステータスに応じた動作は以下の通り:
  - **429 (Too Many Requests)**: `Retry-After` ヘッダーが存在し、かつ aggregate sidecar budget の残時間内に収まる場合は 1 回だけ待機・再送する。`Retry-After` がない、または待機後も 429 が返る場合は当該 request をスキップする
  - **5xx (Server Error)**: リトライせずに当該 request をスキップする
  - **その他のエラー応答（4xx 等、429 を除く）**: リトライせずに当該 request をスキップする。`stderr` / 構造化ログへ warning（HTTP ステータスコードを含む）を出力する
  - スキップされた request の診断はテンプレート提案のみ返す。コア診断・スコア・Exit code は不変
- **Aggregate sidecar budget**: 1 回の `kalos check` 実行全体で LLM sidecar に費やす**壁時間（wall-clock time）**の上限は `120s`（暫定値）とする。v1 は逐次実行のため、各 request の所要時間（429 の Retry-After 待機を含む）が累積される。最初の request 送信開始から最後の response 受信完了（またはスキップ決定）までの経過壁時間で会計する。上限到達後は残りの `LlmEnrichmentRequest` をスキップし、テンプレート提案のみ返す。コア診断・スコア・Exit code は不変。上限超過を `stderr` / 構造化ログへ warning として出力する。暫定値は PoC フィードバックに基づき v1 リリースまでに確定する
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
- **プロバイダ選択**: 環境変数 `KALOS_LLM_PROVIDER` でプロバイダを指定する（v1 の許容値: `openai`、デフォルト: `openai`）。プロバイダはリクエスト形式とデフォルトエンドポイント URL を決定する
- **Outbound 通信**: `--llm` は LLM エンドポイントへのネットワークアクセスを暗示する（REQ-NF-009）。エンドポイント URL は環境変数 `KALOS_LLM_ENDPOINT_URL` で設定する。未設定時は `KALOS_LLM_PROVIDER` で決まるプロバイダ固有のデフォルト URL を使用する（例: `openai` → `https://api.openai.com/v1`）。接続先エンドポイント URL は info レベルでログ出力する（ペイロードは出力しない）。**URL 秘匿化契約**: ログ出力時はスキーム・ホスト・パスのみを記録し、クエリパラメータとフラグメントは除去する。これにより、URL にトークンや API キーが含まれる場合の資格情報漏えいを防ぐ。認証情報は `Authorization` ヘッダー経由で送信し、URL に埋め込まない運用を推奨する
- **データ機密性**: `source_excerpt` / `cpg_excerpt` はプロプライエタリコードを含む可能性がある。`--llm` の指定をもってユーザーの明示的オプトインとする
- **監査境界**: リクエスト/レスポンスのメタデータ（タイムスタンプ、トークン数、ステータスコード）は debug レベルで構造化ログに出力する。コンテンツ自体はログに含めない

### リスク

- LLM が冗長または不正確な提案を返す可能性があるため、テンプレート結果を常に併記し、ユーザーが比較できるようにする

## 改訂履歴

> **凡例**: `関連文書版` は、当該改訂の時点で整合性を確認した各文書の版を示す（変更が導入された版ではなく、確認の対象とした版）。`arch` は architecture.md、`req` は requirements.md を指す。requirements.md の版は architecture.md メタ情報の `入力` フィールドから導出する。ADR 改訂日と各文書版の作成日は一致しない場合がある。

| 日付 | 変更概要 | 関連文書版 |
|---|---|---|
| 2026-03-18 | 初版承認 | arch v0.1.0 / req v0.1.0 |
| 2026-03-19 | `LlmEnrichmentRequest` allowlist 設計追加、sidecar budget（connect/overall timeout）追加、preflight 条件（言語解決不可・multi-file 断片還元不可）追加 | arch v0.2.0–v0.2.11 / req v0.2.0–v0.2.11 |
| 2026-03-21 | レビュー指摘解決: LLM 運用帰結（API キー管理・プロバイダ選択・outbound 通信・データ機密性・監査境界）追加 | arch v0.3.0–v0.3.1 / req v0.3.0 |
| 2026-03-22 | レビュー指摘解決: aggregate sidecar budget（120s 暫定値）追加、v1 ディスパッチポリシー（逐次実行・429/5xx ステータス別処理）追加、URL 秘匿化契約追加 | arch v0.4.0–v0.4.3 / req v0.4.0–v0.4.3 |
| 2026-03-22 | unsupported `KALOS_LLM_PROVIDER` の preflight failure（exit code 2）追加 | arch v0.4.4 / req v0.4.4 |
| 2026-03-26 | レビュー指摘解決: 非 429/5xx HTTP エラーの no-retry+skip ポリシー明記、C/C++ 例を v1 対象言語に即した forward compatibility 記述に置換、ADR-0002 相互参照追加 | arch v0.4.5 / req v0.4.5 |
| 2026-03-26 | レビュー指摘解決: 決定論性契約との関係を明示し、`llm_suggestion` が ADR-0003 の適用範囲外であることを追記 | arch v0.4.7 / req v0.4.5 |
| 2026-03-27 | レビュー指摘解決: 決定論性契約追記の `関連文書版` を ADR-0003 と整合（v0.4.7）、`関連文書版` の凡例を改訂（requirements.md 追跡の追加・意味論の明確化） | arch v0.4.9 / req v0.4.7 |
