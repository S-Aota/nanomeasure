# testdata

動作確認用の合成画像です。実写の TEM 画像ではなく、`examples/gen_testdata.rs` が
固定シードの疑似乱数で生成しています。作り直すには:

```
cargo run --release --example gen_testdata
```

TIFF は無圧縮で書かれるため、リポジトリに置く都合でサイズは控えめにしてあります。

| ファイル | 形式 | サイズ | スケール | 確認したいこと |
|---|---|---|---|---|
| `nanoparticles_16bit.tif` | 16bit TIFF | 1024² | 1 px = 0.09352 nm | 主なテスト画像。支持膜の上の暗いナノ粒子（直径 2.6〜11 nm 相当）。輝度は 12bit 相当（0〜4095）しか使っていないので、**表示の自動コントラストを切ると暗く見える**のが正しい挙動。レベル補正・将来の測長向け |
| `lattice_fringes_16bit.tif` | 16bit TIFF | 512² | 1 px = 0.02703 nm | 23° 傾いた格子縞（格子間隔 ≒ 0.2 nm）。**画像回転の角度確認**に使いやすい |
| `carbon_support_8bit.tif` | 8bit TIFF | 1024² | 1 px = 0.5 × 0.48 nm | アモルファスカーボンの粒状感のみ。**8bit → 16bit 拡張**と、**x/y で画素サイズが違う場合**の確認 |
| `low_contrast_12bit_in_16bit.png` | 16bit PNG | 512² | なし | 輝度が 900〜1500 の狭い範囲にしかない画像。**レベル補正**の効きとヒストグラムの見え方の確認 |
| `color_sample.png` | 8bit RGB PNG | 256² | なし | 白黒以外を読ませたときに**グレースケール変換**されることの確認 |

## メタデータ

TIFF には FEI / Thermo Fisher 形式の私的タグ 34682（`FEI_HELIOS`）を書き込んでいます。
中身は INI 形式のテキストで、`[Scan]` の `PixelWidth` / `PixelHeight` が
**メートル単位**の画素サイズです。

```
[Beam]
Beam=EBeam
Scan=EScan
[Scan]
InternalScan=true
PixelWidth=9.3517e-011
PixelHeight=9.3517e-011
```

これらの TIFF を開くと、コマンドリストに「画像挿入」の次へ「スケール設定」が
自動で追加されます。PNG の 2 枚はスケール情報を持たないので何も追加されず、
1 px = 1 nm のまま扱われます（メタデータが無いときの確認用）。
