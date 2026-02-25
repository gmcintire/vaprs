// vaprs dashboard — EventSource client with DOM updates

(function () {
  'use strict';

  var paused = false;
  var autoScroll = true;
  var state = null;
  var evtSource = null;
  var clockTimer = null;
  var sourceFilter = 'all'; // 'all', 'rf', 'aprsis'

  // ── DOM refs ──
  var el = {
    mycall:     document.getElementById('mycall'),
    version:    document.getElementById('version'),
    uptime:     document.getElementById('uptime'),
    clock:      document.getElementById('clock'),
    aprsisInd:  document.getElementById('aprsis-indicator'),
    aprsisStatus: document.getElementById('aprsis-status'),
    aprsisServer: document.getElementById('aprsis-server'),
    ifaceCount: document.getElementById('interface-count'),
    ifaceNames: document.getElementById('interface-names'),
    stationsHeard: document.getElementById('stations-heard'),
    traffic:    document.getElementById('traffic'),
    packetFeed: document.getElementById('packet-feed'),
    pauseBtn:   document.getElementById('pause-btn'),
    erlangTbody: document.querySelector('#erlang-table tbody'),
    stationsTbody: document.querySelector('#stations-table tbody'),
    igRxRf:     document.getElementById('ig-rx-rf'),
    igGated:    document.getElementById('ig-gated'),
    igRate:     document.getElementById('ig-rate'),
    igUnique:   document.getElementById('ig-unique'),
    igDropped:  document.getElementById('ig-dropped'),
    igDropQuery: document.getElementById('ig-drop-query'),
    igDropSrc:  document.getElementById('ig-drop-src'),
    igDropDst:  document.getElementById('ig-drop-dst'),
    igDropVia:  document.getElementById('ig-drop-via'),
    igDropDepth: document.getElementById('ig-drop-depth'),
  };

  // ── Formatting helpers ──

  function pad2(n) { return n < 10 ? '0' + n : '' + n; }

  function formatUptime(secs) {
    var d = Math.floor(secs / 86400);
    var h = Math.floor((secs % 86400) / 3600);
    var m = Math.floor((secs % 3600) / 60);
    if (d > 0) return d + 'd ' + h + 'h ' + m + 'm';
    if (h > 0) return h + 'h ' + m + 'm';
    return m + 'm';
  }

  function formatTime(epoch) {
    var d = new Date(epoch * 1000);
    return pad2(d.getHours()) + ':' + pad2(d.getMinutes()) + ':' + pad2(d.getSeconds());
  }

  function formatClock() {
    var d = new Date();
    return pad2(d.getHours()) + ':' + pad2(d.getMinutes()) + ':' + pad2(d.getSeconds());
  }

  function formatAgo(secs) {
    if (secs < 60) return secs + 's ago';
    if (secs < 3600) return Math.floor(secs / 60) + 'm ago';
    if (secs < 86400) return Math.floor(secs / 3600) + 'h ago';
    return Math.floor(secs / 86400) + 'd ago';
  }

  function formatPosition(pos) {
    if (!pos) return '\u2014';
    return pos[0].toFixed(2) + ', ' + pos[1].toFixed(2);
  }

  function escapeHtml(s) {
    var div = document.createElement('div');
    div.appendChild(document.createTextNode(s));
    return div.innerHTML;
  }

  // ── Render full state ──

  function renderState(s) {
    state = s;

    el.mycall.textContent = s.mycall;
    el.version.textContent = s.version ? 'v' + s.version : '';
    document.title = 'vaprs \u2014 ' + s.mycall;
    el.uptime.textContent = 'Up: ' + formatUptime(s.uptime_secs);

    // APRS-IS
    if (s.aprsis_connected) {
      el.aprsisInd.className = 'indicator connected';
      el.aprsisStatus.textContent = 'Connected';
    } else {
      el.aprsisInd.className = 'indicator disconnected';
      el.aprsisStatus.textContent = 'Disconnected';
    }
    el.aprsisServer.textContent = s.aprsis_server || '';

    // Interface count and names
    el.ifaceCount.textContent = s.interfaces.length + ' active';
    var ifaceHtml = '';
    for (var i = 0; i < s.interfaces.length; i++) {
      var ifc = s.interfaces[i];
      ifaceHtml += '<div class="iface-entry">' +
        '<strong>' + escapeHtml(ifc.name) + '</strong>' +
        (ifc.detail ? ' <span class="iface-detail">' + escapeHtml(ifc.detail) + '</span>' : '') +
        '</div>';
    }
    el.ifaceNames.innerHTML = ifaceHtml || '\u2014';

    // Stations
    el.stationsHeard.textContent = s.stations_heard + ' heard';

    // Traffic
    el.traffic.textContent = 'Rx: ' + s.rx_per_min + '  Tx: ' + s.tx_per_min;

    // iGate stats
    renderIgateStats(s.igate_stats);

    // Erlang table
    renderErlang(s.erlang_stats);

    // Stations table
    renderStations(s.stations);

    // Packets — full rebuild on state event
    renderPacketFeed(s.recent_packets);
  }

  function renderIgateStats(ig) {
    if (!ig) return;
    el.igRxRf.textContent = ig.rx_from_rf;
    el.igGated.textContent = ig.gated_to_aprsis;
    var rate = ig.rx_from_rf > 0
      ? ((ig.gated_to_aprsis / ig.rx_from_rf) * 100).toFixed(1)
      : '0.0';
    el.igRate.textContent = rate + '%';
    el.igUnique.textContent = ig.unique_stations_gated;
    el.igDropped.textContent = ig.dropped_total;
    el.igDropQuery.textContent = ig.dropped_query;
    el.igDropSrc.textContent = ig.dropped_forbidden_source;
    el.igDropDst.textContent = ig.dropped_forbidden_dest;
    el.igDropVia.textContent = ig.dropped_forbidden_via;
    el.igDropDepth.textContent = ig.dropped_depth_exceeded;
  }

  function renderErlang(stats) {
    if (!stats || stats.length === 0) {
      el.erlangTbody.innerHTML = '<tr><td colspan="6" class="table-empty">No erlang data yet</td></tr>';
      return;
    }
    var html = '';
    for (var i = 0; i < stats.length; i++) {
      var ch = stats[i];
      var cur = ch.current || {rx_packets: 0, tx_packets: 0};
      html += '<tr>' +
        '<td class="iface">' + escapeHtml(ch.name) + '</td>' +
        '<td class="num">' + cur.rx_packets + '/' + cur.tx_packets + '</td>' +
        '<td class="num">' + ch.last_1min.rx_packets + '/' + ch.last_1min.tx_packets + '</td>' +
        '<td class="num">' + ch.last_10min.rx_packets + '/' + ch.last_10min.tx_packets + '</td>' +
        '<td class="num">' + ch.last_60min.rx_packets + '/' + ch.last_60min.tx_packets + '</td>' +
        '<td class="num">' + ch.last_60min.drops + '</td>' +
        '</tr>';
    }
    el.erlangTbody.innerHTML = html;
  }

  function renderStations(stations) {
    if (!stations || stations.length === 0) {
      el.stationsTbody.innerHTML = '<tr><td colspan="5" class="table-empty">No stations heard</td></tr>';
      return;
    }
    var html = '';
    var limit = Math.min(stations.length, 20);
    for (var i = 0; i < limit; i++) {
      var st = stations[i];
      html += '<tr>' +
        '<td class="callsign">' + escapeHtml(st.callsign) + '</td>' +
        '<td>' + formatAgo(st.last_heard_secs_ago) + '</td>' +
        '<td class="iface">' + escapeHtml(st.interface) + '</td>' +
        '<td class="num">' + st.heard_count + '</td>' +
        '<td class="pos">' + formatPosition(st.position) + '</td>' +
        '</tr>';
    }
    el.stationsTbody.innerHTML = html;
  }

  // ── Packet feed ──

  function matchesFilter(pkt) {
    if (sourceFilter === 'all') return true;
    if (sourceFilter === 'aprsis') return pkt.interface === 'APRSIS';
    return pkt.interface !== 'APRSIS'; // rf
  }

  function makePacketHtml(pkt) {
    var cls = pkt.interface === 'APRSIS' ? 'from-aprsis' : 'from-rf';
    return '<div class="packet-line ' + cls + '">' +
      '<span class="pkt-time">' + formatTime(pkt.timestamp) + '</span>' +
      '<span class="pkt-call">' + escapeHtml(pkt.source_call) + '</span>' +
      '<span class="pkt-iface">' + escapeHtml(pkt.interface) + '</span>' +
      '<span class="pkt-data">' + escapeHtml(pkt.raw) + '</span>' +
      '</div>';
  }

  function renderPacketFeed(packets) {
    if (!packets || packets.length === 0) {
      el.packetFeed.innerHTML = '<div class="packet-feed-empty">Waiting for packets\u2026</div>';
      return;
    }
    var html = '';
    for (var i = 0; i < packets.length; i++) {
      if (matchesFilter(packets[i])) {
        html += makePacketHtml(packets[i]);
      }
    }
    if (html === '') {
      el.packetFeed.innerHTML = '<div class="packet-feed-empty">No packets match filter</div>';
      return;
    }
    el.packetFeed.innerHTML = html;
    if (autoScroll && !paused) {
      el.packetFeed.scrollTop = el.packetFeed.scrollHeight;
    }
  }

  function appendPacket(pkt) {
    if (paused) return;
    if (!matchesFilter(pkt)) return;

    // Remove empty placeholder if present
    var empty = el.packetFeed.querySelector('.packet-feed-empty');
    if (empty) empty.remove();

    el.packetFeed.insertAdjacentHTML('beforeend', makePacketHtml(pkt));

    // Cap DOM nodes
    while (el.packetFeed.children.length > 200) {
      el.packetFeed.removeChild(el.packetFeed.firstChild);
    }

    if (autoScroll) {
      el.packetFeed.scrollTop = el.packetFeed.scrollHeight;
    }
  }

  // ── Pause / scroll detection ──

  el.pauseBtn.addEventListener('click', function () {
    paused = !paused;
    if (paused) {
      el.pauseBtn.textContent = '\u25B6 PLAY';
      el.pauseBtn.classList.add('paused');
    } else {
      el.pauseBtn.textContent = '\u23F8 PAUSE';
      el.pauseBtn.classList.remove('paused');
      el.packetFeed.scrollTop = el.packetFeed.scrollHeight;
      autoScroll = true;
    }
  });

  // ── Source filter ──

  var filterBtns = document.querySelectorAll('.filter-btn');
  for (var i = 0; i < filterBtns.length; i++) {
    filterBtns[i].addEventListener('click', function () {
      sourceFilter = this.getAttribute('data-filter');
      for (var j = 0; j < filterBtns.length; j++) {
        filterBtns[j].classList.remove('active');
      }
      this.classList.add('active');
      // Re-render with current state
      if (state && state.recent_packets) {
        renderPacketFeed(state.recent_packets);
      }
    });
  }

  el.packetFeed.addEventListener('scroll', function () {
    var feed = el.packetFeed;
    autoScroll = (feed.scrollTop + feed.clientHeight >= feed.scrollHeight - 20);
  });

  // ── Clock ──

  function tickClock() {
    el.clock.textContent = formatClock();
  }
  tickClock();
  clockTimer = setInterval(tickClock, 1000);

  // ── SSE connection ──

  function connect() {
    if (evtSource) {
      evtSource.close();
    }

    evtSource = new EventSource('/api/events');

    evtSource.addEventListener('state', function (e) {
      try {
        var data = JSON.parse(e.data);
        renderState(data);
      } catch (err) {
        console.error('Failed to parse state event:', err);
      }
    });

    evtSource.addEventListener('packet', function (e) {
      try {
        var pkt = JSON.parse(e.data);
        appendPacket(pkt);
      } catch (err) {
        console.error('Failed to parse packet event:', err);
      }
    });

    evtSource.onerror = function () {
      el.aprsisInd.className = 'indicator disconnected';
      // EventSource reconnects automatically
    };
  }

  // ── Init ──

  el.pauseBtn.textContent = '\u23F8 PAUSE';
  el.packetFeed.innerHTML = '<div class="packet-feed-empty">Connecting\u2026</div>';

  connect();
})();
