# ADR-0002: `ExtractorPort` の背後に初期実装として CodeQL Adapter を置く

## ステータス

承認済み

## コンテキスト

要件では CPG 抽出エンジンとして CodeQL を前提としつつ、代替エンジンの比較検討余地も残している。さらに、新言語追加時は CPG 抽出境界内の parser / normalizer / language profile 追加で対応できること、およびクリーン環境でも手動インストールなしで初回実行できることが要求される。

- `REQ-FUNC-001`〜`REQ-FUNC-007`
- `REQ-FUNC-031`
- `REQ-NF-005`
- `REQ-NF-007`
- 要件書 5章「PoC / 将来拡張で検証する項目」

ドメインモデル上も、`ExtractorPort` の外部公開契約は `SourceAnalysis` として定義され、`UnifiedCpg` はその内部公開言語として位置付けられている。

## 検討した選択肢

### 選択肢 A: CodeQL に直接依存する

アプリケーション層から CodeQL CLI / DB へ直接アクセスする。

- 利点:
  - 初期実装が最も簡単
  - 要件の「CodeQL 前提」に素直
- 欠点:
  - 性能問題や代替検討時の変更範囲が広い
  - `REQ-NF-005` の拡張性を損ないやすい

### 選択肢 B: `ExtractorPort` を定義し、初期アダプタとして CodeQL を採用する

抽出エンジンはポート背後へ隠蔽し、`ExtractorPort` の出力契約を `SourceAnalysis`（`UnifiedCpg` + `source_files` + 抑制コメント + 解析警告を束ねる集約ルート）に固定する。`source_files` は workspace-relative path をキーとする決定論的なソースファイルメタデータ対応表であり、下流コンテキストおよび LLM sidecar の `language` 解決の source of truth となる。

- 利点:
  - CodeQL 前提を守りながら将来差し替え可能
  - 言語ごとの差異をアダプタ層へ閉じ込められる
  - 下流コンテキストは `SourceAnalysis` の公開契約のみに依存し、抽出エンジンの詳細を知らない
- 欠点:
  - 抽象化の実装コストが増える
  - 初期 PoC の対象が増える

### 選択肢 C: 言語ごとに別エンジンを最初から使い分ける

Python/TS/Rust/Go で最適エンジンを変える。

- 利点:
  - 言語ごとの最適化余地がある
- 欠点:
  - 初期リリースで複雑すぎる
  - 決定論性と比較可能性を崩しやすい

## 判断

選択肢 B を採用する。

## 根拠

- CodeQL は初期実装として使うが、`ExtractorPort` の出力契約は `SourceAnalysis` に固定する。`SourceAnalysis` は `UnifiedCpg` に加え `source_files`（workspace-relative path をキーとし path 昇順で列挙される決定論的対応表）を含む。下流コンテキストは `SourceAnalysis` 内の `UnifiedCpg` を公開言語として参照し、LLM sidecar の `language` 解決は `source_files` を source of truth とする
- 外部依存の型情報・シグネチャ解決も extractor 境界内の language-specific resolver adapters へ閉じ込め、依存定義・lockfile・ローカル stub / metadata だけで解決する。解決失敗は `SourceAnalysis.warnings` として下流へ渡す
- CodeQL bundle は Managed Tool Cache Adapter が固定バージョン + SHA-256 検証付きで bootstrap / verify / cache し、CLI 利用者へ手動セットアップを要求しない（`REQ-FUNC-031`, `REQ-NF-009`）
- GitHub Action は managed tool cache を prewarm / restore/save する wrapper に留め、bootstrap の正本は kalos CLI 側に置く
- これにより、将来の性能問題や言語追加時の変更を抽出境界内へ閉じ込められる
- ドメインモデルの `LanguageExtension` と整合し、言語固有概念の差異をコアへ漏らさずに済む

## 帰結

### ポジティブ

- 下流コンテキストは CodeQL 非依存で保てる
- ローカル実行と GitHub Action が同じ bootstrap 経路（managed tool cache）を共有できる
- 性能 PoC に失敗しても代替エンジンへ移行しやすい
- 新言語追加時の変更面を限定できる

### ネガティブ

- `ExtractorPort` とマッパーの保守が必要
- CodeQL の表現差異を吸収する正規化ロジックが増える

### 制約

- CodeQL bundle は固定バージョンを managed tool cache へ初回取得し、SHA-256 で検証する。バージョンと checksum の正本は kalos リリースに同梱される managed bundle manifest とする（`REQ-NF-009`）
- managed bundle がキャッシュ済み（warm）かつ `--llm` 未使用であれば、オフライン環境でも `kalos check` が動作する（`REQ-NF-010`）
- bundle 未取得かつオフラインの場合は、bootstrap が必要であることを示す明確なエラーメッセージを出力し exit code 2 で終了する（`REQ-NF-010`）
- 外部シンボル解決は解析時に追加ネットワーク通信を行わず、依存定義・lockfile・ローカル stub / metadata だけを参照する（`REQ-FUNC-007`, `REQ-NF-009`）

### リスク

- CodeQL の実行時間が `REQ-NF-001` を満たさない可能性があるため、PoC に失敗した場合の代替案比較を継続する
