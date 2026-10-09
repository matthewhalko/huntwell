// Something to look at while the app's script downloads (it is large, and
// phones are slow): a skeleton shaped like the page this address will be, in
// the theme theme.js set. React replaces it on its first render. Classes and
// structure are the ones src/components/Skeleton.tsx renders, so the hand-over
// is seamless; styles.css (a <link> in <head>) styles both. Loaded right after
// #root as a file, because the CSP (script-src 'self') blocks inline scripts.
(function () {
  var p = location.pathname, s = '<span class="skel"></span>';
  function rep(n, html) { var o = ''; for (var i = 0; i < n; i++) o += html; return o }
  var rows = '<div class="skel-rows">' + rep(7, '<div class="skel-row">' + rep(4, s) + '</div>') + '</div>';
  var head = '<div class="skel-head"><div><span class="skel skel-title"></span><span class="skel skel-sub"></span></div><span class="skel skel-btn"></span></div>';
  var html;
  if (/^\/app(\/|$)/.test(p)) {
    html =
      '<div class="shell"><div class="topbar"><span class="topbar-brand"><span class="grad-text spark">\u2726</span><span>huntwell</span></span></div>' +
      '<aside class="rail"><nav class="rail-nav">' + rep(8, '<span class="rail-item"><span class="tile skel"></span><span class="rlbl skel"></span></span>') + '</nav></aside>' +
      '<div class="pane"><div class="content">' + head + rows + '</div></div></div>';
  } else if (/^\/(login|signup|forgot|verify|join)(\/|$)/.test(p)) {
    html = '<div class="skel-auth"><div class="card raised"><span class="skel skel-title"></span>' + s + s + '<span class="skel skel-btn"></span></div></div>';
  } else {
    html =
      '<div class="landing"><header class="site-head"><span class="brand" style="color:var(--text)"><span class="grad-text">\u2726</span><span class="word">huntwell</span></span>' +
      '<div class="skel-nav">' + rep(4, s) + '</div><span class="skel skel-btn"></span></header>' +
      '<div class="skel-hero"><span class="skel skel-title"></span><span class="skel skel-title"></span><span class="skel skel-sub"></span><span class="skel skel-sub"></span><span class="skel skel-btn"></span></div></div>';
  }
  var root = document.getElementById('root');
  root.setAttribute('aria-busy', 'true');
  root.innerHTML = html;
})();
