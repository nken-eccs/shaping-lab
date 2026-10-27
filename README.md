# shaping-lab

独立したRustシェーピングライブラリと実験環境
本体は`core/`でHarfRust crateへの依存なし
HarfRust 0.15.0のソースを基にフォントとプランとメトリクスを再利用

## 使い方

本体だけならRust 1.95以降
比較と計測にはC++17コンパイラとPython 3.9以降

```sh
cargo test --locked -p shaping-core
python3 lab.py fetch
cargo test --locked --workspace
cargo run --release --locked -- compare
python3 lab.py bench first --corpus corpus/diagnostic.toml
```

## 配置

| 場所 | 内容 |
|---|---|
| [core/src/reusable.rs](core/src/reusable.rs) | 繰り返し処理のAPIとキャッシュ |
| [core/src/shape.rs](core/src/shape.rs) | シェーピング全体の流れ |
| [core/src/shaper_font.rs](core/src/shaper_font.rs) | フォント表とメトリクス |
| [core/src/buffer.rs](core/src/buffer.rs) | バッファとクラスタ |
| [core/src/ot/layout.rs](core/src/ot/layout.rs) | 候補走査とルックアップ実行 |
| [core/src/ot/lookup/](core/src/ot/lookup/) | GSUBとGPOS |
| [core/src/ot/shaper/](core/src/ot/shaper/) | 言語別の処理 |
| [core/src/planning.rs](core/src/planning.rs) | プラン構築 |
| [core/src/unicode/](core/src/unicode/) | Unicode判定と正規化用の表 |
| [src/engine/](src/engine/) | ベンチマーク用の3エンジン |
| [src/corpus.rs](src/corpus.rs) | 入力とフォント読み込み |
| [src/main.rs](src/main.rs) / [src/report.rs](src/report.rs) | CLIと出力比較 |
| [benches/shaping.rs](benches/shaping.rs) | 全体と段階別と入力切り替えの計測 |
| [tests/conformance.rs](tests/conformance.rs) | 出力一致とキャッシュの検証 |
| [lab.py](lab.py) | 取得と計測とプロファイルと公式ベンチマーク |
| [corpus/](corpus/) | 基本18ケースと対照5ケースと24言語の長文 |
| `vendor/` / `results/` | 取得物と計測結果 |

## 組み込み

利用するプロジェクトのCargo.toml

```toml
[dependencies]
shaping-core = { path = "../shaping/core" }
anyhow = "1"
```

フォント読み取りはread-fontsを使用
本体のビルドと単体テストにHarfBuzzや取得済みフォントは不要

## API

```rust,no_run
use shaping_core as shaping;
use shaping_core::reusable::{Context, Font, Properties, Shaper};
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let bytes = Arc::new(std::fs::read("vendor/fonts/notosans.ttf")?);
    let raw = shaping::Font::new(shaping::font::Blob::Shared(bytes), 0)
        .ok_or_else(|| anyhow::anyhow!("Invalid font"))?;
    let font = Font::new(raw);
    let run = font.prepare(Properties {
        direction: shaping::Direction::LeftToRight,
        script: Some("Latn".parse().unwrap()),
        language: Some("en".parse().unwrap()),
        ..Default::default()
    })?;
    let mut shaper = Shaper::default();
    for text in ["office", "office work", "work"] {
        let output = shaper.shape(&run, text, Context::default())?;
        println!("{}", output.len());
    }
    Ok(())
}
```

`Font`は1つのfaceと可変軸座標に対応
同じインスタンスを複数の`PreparedRun`で共有
別フォントのランも同じ`Shaper`に順次渡せる
出力は次の処理まで借用可能

| 保持対象 | 既定の動作 |
|---|---|
| プラン | フォントごとに直近16件を保持 |
| 可変フォントの字幅 | 非既定座標でグリフごとに保存 |
| 縦書き原点 | グリフごとに保存 |
| バッファ | 入力変更後も容量を再利用 |

メトリクスは256グリフ単位で必要なページだけ確保
`CacheOptions`で個別に無効化でき `cache_stats`で保持数を確認
`clear_caches`で値を破棄でき 保持中の`PreparedRun`は引き続き使用可能
保持中のランが参照するプランはランの解放まで存続

大きいCoverageはグリフごとの索引と不一致を保存
1つのキャッシュは256エントリで512バイト
Coverageはフォント本体の解放まで保持し `clear_caches`の対象外
ペア位置調整はグリフごとのサブテーブル候補も保持

ルックアップの逆引きはプランと一緒に保持
候補位置と変更位置の作業領域は必要時に確保して再利用

座標系は各フォントのUPEM
フォントと方向と文字体系と言語を確定したランを入力
Bidi解析とフォント選択と改行は前段で処理
可変軸の変更は別の`Font`として作成
並列処理ではワーカーごとに`Font`と`Shaper`を保持

## バッチ

`reusable::BatchShaper`で短いRunをまとめて処理
フォントと設定は生成時に渡し テキストと範囲は呼び出しごとに指定

| 型 | 内容 |
|---|---|
| `RunConfig` | フォント番号と`Properties` |
| `RunInput` | 設定番号と共有文字列とUTF-8バイト範囲 |
| `BatchOutput` | 入力順の結果 `iter`と`get`で取得 |
| `RunOutput` | `infos`と`positions`の借用 |

ワーカーは常駐し フォントとプランと作業バッファを保持
フォントのバイト列と解析済みテーブルは共有
worker数は呼び出し元を含む上限値
既定は1ワーカーあたり32Run以上で分配
64Run未満は呼び出し元で処理
閾値は`set_min_runs_per_worker`で変更
プランは初回利用から保持し バッチでは使用する設定だけ準備

文字列は`Arc<str>`で共有し 範囲外を前後文脈に使用
クラスタは指定範囲の先頭からのUTF-8バイト位置
機能の適用範囲も各Runの先頭を基準
結果はワーカーごとの連続配列にコピーし 次の呼び出しで容量を再利用
依存crateの追加なし

```rust,no_run
use shaping_core::reusable::{BatchShaper, Properties, RunConfig, RunInput};
use std::sync::Arc;

fn shape_many(font: shaping_core::Font) -> anyhow::Result<()> {
    let configs = vec![RunConfig { font: 0, properties: Properties::default() }];
    let mut shaper = BatchShaper::new(vec![font], configs, 4)?;
    let text: Arc<str> = Arc::from("office work");
    let inputs: Arc<[RunInput]> = (0..2048).map(|_| RunInput {
        config: 0, text: text.clone(), range: 0..text.len(),
    }).collect();
    let output = shaper.shape(inputs)?;
    for run in output.iter() {
        println!("{}", run.infos.len());
    }
    Ok(())
}
```

## 操作

| 目的 | コマンド |
|---|---|
| ケース一覧 | `cargo run --locked -- list` |
| 出力比較 | `cargo run --release --locked -- compare` |
| 追加キャッシュの比較 | `cargo run --release --locked -- compare --left local` |
| 変更前後の計測 | `python3 lab.py bench after --case mixed-fonts --baseline first-mixed` |
| 入力切り替え | `python3 lab.py bench stream --suite stream --case mixed-fonts` |
| 段階別計測 | `python3 lab.py bench stages --suite stages --case mixed-fonts --split-runs` |
| 初回費用 | `python3 lab.py bench cold --case japanese-vertical --modes reuse,fresh-font` |
| CPUプロファイル | `python3 lab.py profile cpu --case latin-variable --backend optimized` |
| 短いRunの並列処理 | `python3 lab.py bench batches --suite batch --case mixed-fonts` |
| 並列処理のプロファイル | `python3 lab.py profile batch-cpu --suite batch --case mixed-fonts --backend optimized --workers 4` |
| 公式ベンチマーク | `python3 lab.py upstream official` |

基準値は同じ引数で`python3 lab.py bench first-mixed --case mixed-fonts`を先に実行
`--case`省略で全ケース
結果は`results/<名前>/summary.csv`と`criterion/report/index.html`

## 比較対象

| 名前 | 実装 |
|---|---|
| `optimized` | coreの本体と再利用API |
| `local` | 同じ本体でフォント共有と追加キャッシュなし |
| `harfbuzz` | C++のHarfBuzz 15.0.0 |

本体の変更はlocalとoptimizedの両方に反映
未変更のHarfRustは`lab.py upstream`で別に実行

## 取り込み元

HarfRust 0.15.0の[71d96513](https://github.com/harfbuzz/harfrust/tree/71d965130ea93905f81b8f03a03d7ecc794fbc47)
[MITライセンスと著作権表示](core/LICENSE)を保持

本体72ファイルを36ファイルに統合し 再利用APIを1ファイルで追加
stdと組み込みUnicode表に固定し ICUとno_stdの分岐を削除
外部フォントを必要とする上流のテスト群は除外し 合成データのテストと既存コーパスで検証
