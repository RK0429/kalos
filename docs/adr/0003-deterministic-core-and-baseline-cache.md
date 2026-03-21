# ADR-0003: 決定論的コア評価と差分解析用ベースラインキャッシュを採用する

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
  - 総合スコアを「変更後全体」として維持しやすい
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
- `scores.overall` は常にメトリクス集約結果を表し、`WholeProject` / `ListedDiagnostics` は summary と exit code の母集団だけを規定する。`--level all`（デフォルト）では `WholeProject`、`--level function|module|project` では `ListedDiagnostics` を使う。差分モードでもこの契約は変えない
- ただし決定論性を崩さないため、ベースライン識別子（`BaselineFingerprint`）は以下の 7 要素で決定する
  - `workspace_root_hash`: Configuration が `--config <path>` 指定時はその `.kalos.toml` の親を、未指定時は `nearest .kalos.toml parent -> nearest .git parent -> current working directory` の順で解決した `WorkspaceRoot` の正規化絶対パスの SHA-256。同一リポジトリでもクローン場所が異なるとキャッシュを分離する
  - `base_snapshot_hash`: `--diff <base-ref>` の基準側 tree hash。現在ワークツリーのハッシュは含めない
  - `config_hash`: `ProjectConfig`（マージ済み設定）のハッシュ。除外パターンの和集合と正規化済み `plugin_manifest` を含む
- `analysis_targets_hash`: `analysis_targets` の正規化済み path 群から算出したハッシュ。解析対象 path が変わった場合の誤再利用を防ぐ
- `rule_catalog_version`: 組み込みルールカタログの版
- `extractor_version`: 抽出エンジン（CodeQL bundle 等）の版
- `kalos_version`: kalos バイナリ自体の版
- ベースラインの **保存不変条件**: ベースラインは常に全ワークスペース（`config_hash` に含まれる除外パターン適用後の全対象ファイル）かつ全階層の解析結果を保存する。`--level` は報告対象を絞るだけで、保存範囲は変えない。そのため `requested_level` は `BaselineFingerprint` に含めず、異なる `--level` 間でも同じ完全ベースラインを再利用できる
- ベースラインの **永続化対象は全ワークスペース解析に限定** する。`analysis_targets` が全ワークスペースの部分集合である実行は、新たなベースラインを **生成せず**、既存の全ワークスペース baseline も **消費しない**。`analysis_targets_hash` を含む完全一致互換を保つことで、部分 target と全ワークスペースの意味論を混同しない。この場合 `--diff` 最適化は無効化し、要求された `analysis_targets` / `--level` を保った non-diff 全解析へフォールバックする（全ワークスペースへは拡張しない。フォールバック対象は要求された `analysis_targets` のみである）。`--level` は報告対象の制限であり、ベースラインの生成・消費の判定には影響しない
- 差分モードの summary を再構成するため、保存単位は `ScopeMetrics` だけでなく `ScopeDiagnosticSnapshot`、`OverallScore`、`DependencyIndexManifest` を含む。`ScopeDiagnosticSnapshot` は `Diagnostic.primary_scope_id` ごとに診断断片を一意に束ねる
- diff 最適化が有効な限り project スコープは常に再計算対象に含める。project-level metrics と `OverallScore` は merged post-change snapshot から再構成し、baseline の project 断片を最終結果へそのまま流用しない
- プラグインメトリクスのベースライン再利用は、当該プラグインが現在の実行で正常にロード・評価された場合に限る。失敗またはスキップされたプラグインの `MetricValue` は baseline 断片から除外し、stale な report-only plugin metric が部分的に残ることを防ぐ
- `InvalidationPlan.fallback_to_full` は次の場合に `true` となる: `analysis_targets` が全ワークスペースの部分集合で diff 最適化を適用できない、ベースライン不在、`BaselineFingerprint` 不一致または版情報不一致、逆依存閉包から `AffectedScopeSet` を安全に確定できない、または project scope を安全に再計算できない
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
- 保存不変条件により、`analysis_targets` サブセット実行ではベースラインが生成されない。`--level` 限定実行でも全ワークスペース解析であればベースラインは生成できるが、CI で差分解析のベースラインを安定運用するには、定期的な全ワークスペース解析（nightly ビルド等）が必要となる
- ベースラインキャッシュはリポジトリ規模に比例して増大する。v1 では自動 eviction を提供しない
- **CI**: キャッシュは best-effort。CI の cache restore/save メカニズム（GitHub Actions `actions/cache` 等）で管理し、checkout path を安定化させて `workspace_root_hash` のヒット率を高める運用が必要
- **ローカル**: ユーザーがキャッシュディレクトリを手動削除できる。将来の改善候補として LRU eviction またはサイズベースの pruning を検討する
- **保存場所**: `$KALOS_CACHE_DIR`（未設定時は `$XDG_CACHE_HOME/kalos` または `~/.cache/kalos`）

### リスク

- `f64` 集約や外部アダプタ差異でビット一致が壊れる可能性があるため、ハッシュ比較の適合度関数を必須にする
