// OGP 画像（src/web/og.png）とホーム画面のアイコン（src/web/apple-touch-icon.png）を作り直す:
//   NODE_PATH=$(npm root -g) node assets/render-og.js
// og.html は Google Fonts の Noto Sans JP を読み込むので、ネットにつながった環境で実行する
const { chromium } = require('playwright');
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1200, height: 630 } });
  await p.goto('file://' + __dirname + '/og.html', { waitUntil: 'networkidle' });
  await p.evaluate(() => document.fonts.ready);
  console.log('fonts:', await p.evaluate(() => [...document.fonts].filter(f => f.status === 'loaded').length));
  await p.screenshot({ path: __dirname + '/../src/web/og.png' });
  const q = await b.newPage({ viewport: { width: 180, height: 180 } });
  await q.goto('file://' + __dirname + '/apple-touch-icon.html');
  await q.screenshot({ path: __dirname + '/../src/web/apple-touch-icon.png' });
  await b.close();
})();
