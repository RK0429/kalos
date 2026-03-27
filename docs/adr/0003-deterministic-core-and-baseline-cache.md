# ADR-0003: 決定論的コア評価と差分解析用ベースラインキャッシュを採用する

## メタ情報

| 項目 | 内容 |
|---|---|
| 承認日 | 2026-03-18 |
| 最終更新日 | 2026-03-27 |
| 改訂 | v0.4.10 |

> **注記**: メタ情報の `改訂` は本 ADR 自体の版番号であり、改訂履歴の `関連文書版` 列に記載される architecture.md / requirements.md の版番号とは独立したバージョニング体系である。

## ステータス

承認済み

## コンテキスト

kalos は同時に以下を満たす必要がある。

- 同一入力・同一設定でビット単位一致 (`REQ-NF-003`)
- 全解析 60 秒以内 (`REQ-NF-001`)
- 差分解析 10 秒以内 (`REQ-NF-002`)
- `--diff` でも総合スコアと診断サマリーを返す (`REQ-FUNC-024`, `REQ-FUNC-034`)

要件だけでは、差分解析時に総合スコアをどう定義するかは自明ではない。

## 検討した選択肢

### 選択肢 A: 常に全再計算する

- 利点:
  - 意味論が単純
  - キャッシュ整合性問題がない
- 欠点:
  - 差分解析 10 秒以内が難しい

### 選択肢 B: 非変更スコープのベースラインを再利用し、影響範囲のみ再計算する

- 利点:
  - 差分解析を高速化できる
  - 総合スコアを「変更後の解析対象全体」として維持しやすい
- 欠点:
  - キャッシュ無効化規則が必要
  - 実装が複雑になる

### 選択肢 C: 差分解析では部分結果だけ返し、総合スコアを出さない

- 利点:
  - 実装が軽い
  - 速度を出しやすい
- 欠点:
  - `REQ-FUNC-024` と整合しにくい
  - UX が分岐する

## 判断

選択肢 B を採用する。

## 根拠

- `REQ-FUNC-024/034` を両立するには、非変更部分のベースライン再利用が最も自然
- `scores.overall` は常にメトリクス集約結果を表し、`SummaryScope` 列挙型の variant（`WholeProject` / `ListedDiagnostics`）は summary と exit code の母集団だけを規定する。`--level all`（デフォルト）では `SummaryScope::WholeProject`（JSON wire value: `"whole_project"`）、`--level function|module|project` では `SummaryScope::ListedDiagnostics`（JSON wire value: `"listed_diagnostics"`）を使う。差分モードでもこの契約は変えない
- **決定論性契約の適用範囲**: `REQ-NF-003` のビット単位一致は、コア評価パイプライン（CPG 抽出 → メトリクス算出 → 診断生成 → レポート組立）の出力に適用する。具体的には `ScopeMetrics`、`Diagnostic`（重大度を含む）、`OverallScore`、`DiagnosticReport`、Exit code、および評価順序が対象である。`--llm` 指定時に後段で付加される `llm_suggestion`（`LlmSuggestionBundle`）は決定論性契約の適用範囲外とする（ADR-0005 参照）。LLM 応答は本質的に非決定的であり、wall-clock budget やネットワーク状態にも依存するため、同一入力でも `llm_suggestion` の有無・内容は再現性を保証しない
- ただし決定論性を崩さないため、ベースライン識別子（`BaselineFingerprint`）は以下の 7 要素で決定する
  - `workspace_root_hash`: Configuration が `--config <path>` 指定時はその `.kalos.toml` の親を、未指定時は `nearest .kalos.toml parent -> nearest .git parent -> current working directory` の順で解決した `WorkspaceRoot` の正規化絶対パスの SHA-256。同一リポジトリでもクローン場所が異なるとキャッシュを分離する。**正規化規則**: 以下の手順を順に適用した結果の UTF-8 バイト列を SHA-256 でハッシュする
    1. **絶対パス化**: 相対パスの場合は current working directory を基準に絶対パスへ変換する
    2. **シンボリックリンク解決**: 全構成要素のシンボリックリンクを物理パスに解決する（POSIX `realpath` / Rust `std::fs::canonicalize` 相当）。これにより、同一ディレクトリへの異なるシンボリックリンク経由アクセスは同一ハッシュとなる
    3. **`.` / `..` 除去**: 手順 2 で暗黙に除去される
    4. **末尾セパレータ除去**: ルートディレクトリ（`/` または `C:\`）を除き、末尾のパス区切り文字を除去する
    5. **Windows 固有**: ドライブレターを大文字に正規化し、拡張長パスプレフィクス（`\\?\`）を除去する。パス区切り文字はネイティブ `\` を使用する
    - **キャッシュ再利用スコープ**: 正規化の結果、再利用の判定基準は**物理ディレクトリの同一性**となる。同一物理ディレクトリへの異なるシンボリックリンクはキャッシュを共有し、異なる物理ディレクトリ（同一リポジトリの別クローンを含む）はキャッシュを分離する
  - `base_snapshot_hash`: `--diff <base-ref>` の基準側 tree hash。現在ワークツリーのハッシュは含めない
  - `config_hash`: `ProjectConfig`（マージ済み設定）のハッシュ。除外パターンの和集合と正規化済み `plugin_manifest` を含む
  - `analysis_targets_hash`: `analysis_targets` の正規化済み path 群から算出したハッシュ。解析対象 path が変わった場合の誤再利用を防ぐ。**全ワークスペース判定と正規化**: `ProjectConfig.resolve()` は位置引数の省略/明示を `targets_explicitly_specified: bool` として記録する。`targets_explicitly_specified = false`（位置引数省略、デフォルト `["."]`）の場合は全ワークスペースとして扱い、`analysis_targets_hash` は正規形 `["."]` から算出する。`targets_explicitly_specified = true`（位置引数が明示的に指定された場合）は、`WorkspaceRoot` 相対パスへ正規化したうえでソート済み重複排除リストからハッシュを算出する。明示的指定が `WorkspaceRoot` 配下の全対象ファイルを網羅するかどうかは判定しない（明示指定は常に部分集合として扱う）。**`targets_explicitly_specified` は全ワークスペース判定・ベースライン生成/消費・diff 最適化の適用可否を決定する唯一の権威的信号である**（ファイル集合の網羅性比較は行わない）
  - `rule_catalog_version`: 組み込みルールカタログの版
  - `extractor_version`: 抽出エンジン（CodeQL bundle 等）の版
  - `kalos_version`: kalos バイナリ自体の版
- ベースラインの **保存不変条件**: ベースラインは常に全ワークスペース（`config_hash` に含まれる除外パターン適用後の全対象ファイル）かつ全階層の解析結果を保存する。`--level` は報告対象を絞るだけであり、内部的には全階層（function / module / project）のメトリクス算出・診断生成を実行する。保存範囲も変えない。そのため `requested_level` は `BaselineFingerprint` に含めず、異なる `--level` 間でも同じ完全ベースラインを再利用できる
- ベースラインの **write-back 契約**: 書き込み条件は全ワークスペース解析が正常完了した場合のみ（exit code 0 または 1）。書き込みタイミングは `DiagnosticReport` の assemble 完了後、exit code 返却前。一時ファイルへ書き込み後にリネームし、部分書き込みを防ぐ。kalos 自体の実行エラー（exit code 2）では書き込まない。詳細は [architecture.md §5.2](../architecture.md) の write-back 契約を参照
- ベースラインの **永続化対象は全ワークスペース解析に限定** する。`targets_explicitly_specified = true` の実行は、新たなベースラインを **生成せず**、既存の全ワークスペース baseline も **消費しない**。`analysis_targets_hash` を含む完全一致互換を保つことで、部分 target と全ワークスペースの意味論を混同しない。この場合 `--diff` 最適化は無効化し、要求された `analysis_targets` / `--level` を保った **non-diff 全スコープ解析** へフォールバックする（全ワークスペースへは拡張しない。フォールバック対象は要求された `analysis_targets` のみである）。`--level` は報告対象の制限であり、ベースラインの生成・消費の判定には影響しない
- 差分モードの summary を再構成するため、保存単位は `ScopeMetrics` だけでなく `ScopeDiagnosticSnapshot`、`OverallScore`、`DependencyIndexManifest` を含む。コンテキスト間で共有されるこれらの型の配置は ADR-0001 の依存方向ルール（`ports` モジュール）に従う。`ScopeDiagnosticSnapshot` は `Diagnostic.primary_scope_id` ごとに診断断片を一意に束ねる
- diff 最適化が有効な限り project スコープは常に再計算対象に含める。project-level metrics と `OverallScore` は merged post-change snapshot から再構成し、baseline の project 断片を最終結果へそのまま流用しない
- プラグインメトリクスのベースライン再利用は、当該プラグインが現在の実行で正常にロード・評価された場合に限る。失敗またはスキップされたプラグインの `MetricValue` は baseline 断片から除外し、stale な report-only plugin metric が部分的に残ることを防ぐ（ADR-0004 参照）
- **用語の区別**: 本 ADR では「全ワークスペース解析」（full-workspace analysis）を「`WorkspaceRoot` 配下の全対象ファイルを解析する実行」の意味で使い、「non-diff 全スコープ解析」を「要求された `analysis_targets` 内の全スコープを diff 最適化なしで解析する実行」の意味で使う。後者は解析対象を全ワークスペースへ拡張しない
- `targets_explicitly_specified = true` の場合は diff 最適化が**上流で**無効化され、`InvalidationPlan` は生成されない（前項参照）。`InvalidationPlan.fallback_to_full` は**全ワークスペース解析の diff フロー内**で次の場合に `true` となる: ベースライン不在、`BaselineFingerprint` 不一致または版情報不一致、逆依存閉包から `AffectedScopeSet` を安全に確定できない、または project scope を安全に再計算できない。`fallback_to_full = true` は解析対象のファイル集合自体を変更せず、diff 最適化のみを無効化して全スコープを再計算する
- コア評価順序は常に `ScopeId` の辞書順 `(<level>, <qualified_name>, <file_path>)` に固定し、`AnalysisLevel` の順序は `Function < Module < Project` とする。キャッシュヒット時も同じ comparator で統合する

## 帰結

### ポジティブ

- 差分解析性能の目標に現実味が出る
- 総合スコアの意味を維持しやすい
- CI でもローカルでも同じ correctness 戦略を使える
- キャッシュヒット/ミスの判定基準をドキュメントで一意に定義できる

### ネガティブ

- キャッシュ破損や無効化漏れが新たな障害源になる
- 設定変更やプラグイン差し替えで再計算が増える
- `base_snapshot_hash` は `--diff <base-ref>` の基準側 tree hash であり、取得元が曖昧だと再利用判定が壊れるため、`git rev-parse <base-ref>^{tree}` 相当の取得方法を実装で固定する必要がある
- checkout path が実行ごとに変わる CI では `workspace_root_hash` によりキャッシュヒット率が下がる。再利用は best-effort とし、ヒット率を重視する環境では checkout path を安定化させ、baseline cache を restore/save する運用が必要
- 保存不変条件により、`targets_explicitly_specified = true`（サブセット）実行ではベースラインが生成されない。`--level` 限定実行でも全ワークスペース解析であればベースラインは生成できるが、CI で差分解析のベースラインを安定運用するには、定期的な全ワークスペース解析（nightly ビルド等）が必要となる
- ベースラインキャッシュはリポジトリ規模に比例して増大する。v1 では自動 eviction を提供しない
- **CI**: キャッシュは best-effort。CI の cache restore/save メカニズム（GitHub Actions `actions/cache` 等）で管理し、checkout path を安定化させて `workspace_root_hash` のヒット率を高める運用が必要
- **ローカル**: ユーザーがキャッシュディレクトリを手動削除できる。将来の改善候補として LRU eviction またはサイズベースの pruning を検討する
- **保存場所**: `$KALOS_CACHE_DIR`（未設定時のプラットフォーム別既定: Linux/macOS は `$XDG_CACHE_HOME/kalos` または `~/.cache/kalos`、Windows は `%LOCALAPPDATA%\kalos`）

### リスク

- `f64` 集約や外部アダプタ差異でビット一致が壊れる可能性があるため、ハッシュ比較の適合度関数を必須にする

## 改訂履歴

> **凡例**: `関連文書版` は、当該改訂の時点で整合性を確認した各文書の版を示す（変更が導入された版ではなく、確認の対象とした版）。`arch` は architecture.md、`req` は requirements.md を指す。requirements.md の版は architecture.md メタ情報の `入力` フィールドから導出する。ADR 改訂日と各文書版の作成日は一致しない場合がある。

| 日付 | 変更概要 | 関連文書版 |
|---|---|---|
| 2026-03-18 | 初版承認 | arch v0.1.0 / req v0.1.0 |
| 2026-03-19 | `BaselineFingerprint` 7 要素の定義、`SummaryScope` variant の明文化、保存不変条件・write-back 契約追加、コア評価順序の固定規則追加 | arch v0.2.0–v0.2.8 / req v0.2.0–v0.2.8 |
| 2026-03-20 | `ScopeDiagnosticSnapshot` 保存単位、project scope 再計算規則、プラグインメトリクスのベースライン再利用ゲート追加 | arch v0.2.12 / req v0.2.11 |
| 2026-03-21 | レビュー指摘解決: subset fallback 文言修正 | arch v0.3.0 / req v0.3.0 |
| 2026-03-22 | レビュー指摘解決: キャッシュ運用帰結（CI / ローカル / 保存場所）追加、用語の区別（全ワークスペース解析 vs non-diff 全スコープ解析）明文化、`InvalidationPlan` 仕様・`targets_explicitly_specified` 契約追記 | arch v0.4.0–v0.4.5 / req v0.4.0–v0.4.5 |
| 2026-03-26 | レビュー指摘解決: プラグインメトリクスのベースライン再利用に ADR-0004 相互参照追加 | arch v0.4.6 / req v0.4.6 |
| 2026-03-26 | レビュー指摘解決: 決定論性契約の適用範囲を明示し、`llm_suggestion` が範囲外であることを ADR-0005 相互参照付きで追記 | arch v0.4.7 / req v0.4.5 |
| 2026-03-27 | レビュー指摘解決: 公開契約型の `ports` 配置参照追記、`関連文書版` の凡例を改訂（requirements.md 追跡の追加・意味論の明確化） | arch v0.4.9 / req v0.4.7 |
| 2026-03-27 | レビュー指摘解決: 選択肢 B の利点記述「変更後全体」を「変更後の解析対象全体」に明確化 | arch v0.4.11 / req v0.4.8 |
| 2026-03-27 | レビュー指摘解決: `workspace_root_hash` の正規化規則（シンボリックリンク解決・末尾セパレータ除去・Windows 固有処理）とキャッシュ再利用スコープを明文化 | arch v0.4.12 / req v0.4.9 |
