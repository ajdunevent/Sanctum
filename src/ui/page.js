(() => {
  if (window.__sanctumPage) return;
  window.__sanctumPage = true;

  const post = (message) => window.webkit.messageHandlers.sanctum.postMessage(message);
  const INTERACTIVE =
    'a,button,input,select,textarea,label,summary,[contenteditable=""],[contenteditable="true"],' +
    '[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=switch],[role=slider],' +
    '[role=textbox],[role=combobox],[role=option],[role=searchbox],[tabindex]:not([tabindex="-1"])';
  const DIALOG =
    'dialog,[role=dialog],[role=alertdialog],ha-dialog,ha-md-dialog,ha-wa-dialog,' +
    'ha-adaptive-dialog,ha-bottom-sheet,ha-snap-bottom-sheet';
  const BAR =
    'header,[role=toolbar],[role=banner],.toolbar,.header,.mdc-top-app-bar,app-toolbar,app-header';
  const NO_PULL =
    'input[type=range],ha-slider,ha-control-slider,ha-control-circular-slider,ha-map,' +
    'ha-code-editor,.leaflet-container,ha-more-info-dialog';

  const length = (name, fallback) => {
    const value = parseFloat(getComputedStyle(document.documentElement).getPropertyValue(name));
    return Number.isFinite(value) ? value : fallback;
  };

  // The sidebar title has no toolbar class, but it sits in the same band.
  const isBar = (node) =>
    node.matches(BAR) ||
    (node.matches('.menu') && node.getRootNode().host?.localName === 'ha-sidebar');

  let reported = null;
  const onToolbar = (event) => {
    if (!window.sanctumTitlebar || reported?.startsWith('false')) return false;
    const height = length('--header-height', 56) + length('--safe-area-inset-top', 0);
    if (event.clientY > height) return false;
    let bar = false;
    for (const node of event.composedPath()) {
      if (!(node instanceof Element)) continue;
      if (node.matches(INTERACTIVE) || node.matches(DIALOG)) return false;
      if (isBar(node)) bar = true;
    }
    return bar;
  };

  // The frontend pads its toolbars by its companion app inset variables. Set
  // on the toolbar alone, they keep its buttons clear of the window buttons
  // while the page below keeps its full width. Toolbars are created per page,
  // so the check repeats.
  const padded = new Set();
  const inBand = (node, band) => {
    const box = node.getBoundingClientRect();
    return (
      Math.abs(box.top) <= 1 &&
      box.bottom >= band / 2 &&
      box.bottom <= band + 1 &&
      box.width >= innerWidth / 2
    );
  };
  const toolbarAt = (x, band) => {
    let el = document.elementFromPoint(x, band / 2);
    while (el?.shadowRoot) {
      const inner = el.shadowRoot.elementFromPoint(x, band / 2);
      if (!inner || inner === el) break;
      el = inner;
    }
    // Only an element lying within the band counts. A scrolled document's
    // root also ends above the band, and padding it indents the whole page.
    let bar = null;
    for (let node = el; node; node = node.assignedSlot || node.parentNode || node.host) {
      if (!(node instanceof Element) || node === document.documentElement || node === document.body) continue;
      if (inBand(node, band)) bar = node;
    }
    return bar;
  };
  // Some toolbars, such as the app panel's, ignore the pointer except on
  // their buttons, so the far edge is tried too, where a menu button sits.
  const barNear = (x, far, band) => toolbarAt(x, band) ?? toolbarAt(far, band);

  // Dialogs, the sidebar drawer and their scrims cover the toolbar without
  // replacing it.
  const OVERLAY = `${DIALOG},dialog,ha-sidebar,.mdc-drawer,.mdc-drawer-scrim,.scrim,[popover]`;
  const overlayAt = (x, y) => {
    let el = document.elementFromPoint(x, y);
    while (el?.shadowRoot) {
      const inner = el.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === el) break;
      el = inner;
    }
    for (let node = el; node; node = node.assignedSlot || node.parentNode || node.host) {
      if (node instanceof Element && node.matches(OVERLAY)) return true;
    }
    return false;
  };

  // Sanctum shows its header on pages without a toolbar it can use, such
  // as an app panel on a wide window. The launch screen has none either, so
  // nothing is reported until it is gone, and a change is reported once it
  // has held for two checks.
  let pending = null;
  const reportToolbar = (bar) => {
    if (document.getElementById('ha-launch-screen')) return;
    const height = bar ? Math.round(bar.getBoundingClientRect().bottom) : 0;
    const key = `${!!bar}:${height}`;
    if (key === reported) {
      pending = null;
      return;
    }
    if (pending !== key) {
      pending = key;
      return;
    }
    reported = key;
    pending = null;
    post({ type: 'toolbar', found: !!bar, height });
  };

  // The toolbar's overflow menu moves into Sanctum's menu, which sits right
  // beside it. The frontend builds these menus with ha-dropdown, whose items
  // exist while it is closed and which reports a choice with wa-select.
  const DOTS =
    'M12,16A2,2 0 0,1 14,18A2,2 0 0,1 12,20A2,2 0 0,1 10,18A2,2 0 0,1 12,16M12,10A2,2 0 0,1 ' +
    '14,12A2,2 0 0,1 12,14A2,2 0 0,1 10,12A2,2 0 0,1 12,10M12,4A2,2 0 0,1 14,6A2,2 0 0,1 ' +
    '12,8A2,2 0 0,1 10,6A2,2 0 0,1 12,4Z';
  let absorbed = null;
  let absorbedItems = [];
  let menuKey = 'null';

  const eachDeep = (node, visit) => {
    visit(node);
    if (node.shadowRoot) eachDeep(node.shadowRoot, visit);
    const children = node.localName === 'slot' ? node.assignedElements({ flatten: true }) : node.children;
    for (const child of children ?? []) eachDeep(child, visit);
  };

  const overflowMenu = (bar, band) => {
    let best = null;
    let right = -1;
    eachDeep(bar, (node) => {
      if (node.localName !== 'ha-dropdown') return;
      const trigger = node.querySelector(':scope > [slot=trigger]');
      if (trigger?.path !== DOTS && node !== absorbed) return;
      const box = (node === absorbed ? node.parentElement ?? node : trigger).getBoundingClientRect();
      if (box.top > band || box.right <= right) return;
      best = node;
      right = box.right;
    });
    return best;
  };

  const itemLabel = (item) =>
    Array.from(item.childNodes)
      .filter((child) => child.nodeType === Node.TEXT_NODE || (child instanceof Element && !child.slot))
      .map((child) => child.textContent)
      .join(' ')
      .replace(/\s+/g, ' ')
      .trim();

  // The section that held the menu keeps its padding once the menu is hidden,
  // leaving a blank gap that squeezes the title, so it is collapsed while
  // nothing in it shows.
  let collapsed = null;
  const shows = (node) => node.getBoundingClientRect().width > 0;
  const collapseHolder = (dropdown) => {
    const holder = dropdown.assignedSlot?.parentElement ?? dropdown.parentElement;
    if (!holder) return;
    const visible = Array.from(holder.children).some((child) =>
      child.localName === 'slot'
        ? child.assignedElements({ flatten: true }).some(shows)
        : child !== dropdown && shows(child),
    );
    if (visible) {
      expandHolder();
    } else if (collapsed !== holder) {
      expandHolder();
      holder.style.setProperty('padding-left', '0');
      holder.style.setProperty('padding-right', '0');
      collapsed = holder;
    }
  };
  const expandHolder = () => {
    collapsed?.style.removeProperty('padding-left');
    collapsed?.style.removeProperty('padding-right');
    collapsed = null;
  };

  // Icon buttons in the toolbar's action slots, such as the search button in
  // Settings, move into the menu too. Their label is the accessible one.
  const ACTION_SLOTS = ['actionItems', 'toolbar-icon'];
  let absorbedButtons = [];
  const actionButtons = (bar, band) => {
    const found = [];
    eachDeep(bar, (node) => {
      if (
        node.localName === 'ha-icon-button' &&
        ACTION_SLOTS.includes(node.slot) &&
        !node.hidden &&
        node.label &&
        (absorbedButtons.includes(node) || (shows(node) && node.getBoundingClientRect().top < band))
      ) {
        found.push(node);
      }
    });
    return found;
  };

  const releaseButtons = (keep = []) => {
    for (const button of absorbedButtons) {
      if (!keep.includes(button)) button.style.removeProperty('display');
    }
    absorbedButtons = keep;
  };

  const releaseMenu = () => {
    absorbed?.style.removeProperty('display');
    absorbed = null;
    releaseButtons();
    expandHolder();
  };

  const absorbMenu = (bar, band) => {
    const dropdown = bar ? overflowMenu(bar, band) : null;
    const buttons = bar ? actionButtons(bar, band) : [];
    if (dropdown !== absorbed) {
      releaseMenu();
      absorbed = dropdown;
    }
    releaseButtons(buttons);
    absorbedItems = [];
    const items = [];
    for (const button of buttons) {
      button.style.setProperty('display', 'none');
      absorbedItems.push(button);
      items.push({
        id: absorbedItems.length - 1,
        label: String(button.label),
        icon: button.path ?? null,
        disabled: !!button.disabled,
        checkbox: false,
        checked: false,
      });
    }
    if (dropdown) {
      if (buttons.length) items.push({ separator: true });
      dropdown.style.setProperty('display', 'none');
      collapseHolder(dropdown);
      for (const child of dropdown.children) {
        if (child.localName === 'wa-divider') {
          items.push({ separator: true });
        } else if (child.localName === 'ha-dropdown-item' && !child.hidden) {
          absorbedItems.push(child);
          items.push({
            id: absorbedItems.length - 1,
            label: itemLabel(child),
            icon: child.querySelector(':scope > [slot=icon]')?.path ?? null,
            disabled: !!child.disabled,
            checkbox: child.type === 'checkbox',
            checked: !!child.checked,
          });
        }
      }
    }
    const shown = dropdown || buttons.length ? items : null;
    const key = JSON.stringify(shown);
    if (key !== menuKey) {
      menuKey = key;
      post({ type: 'menu', items: shown });
    }
  };

  window.sanctumMenuSelect = (id) => {
    const item = absorbedItems[id];
    if (!item?.isConnected) return;
    if (item.localName === 'ha-icon-button') {
      item.click();
      return;
    }
    if (item.type === 'checkbox') item.checked = !item.checked;
    absorbed?.dispatchEvent(
      new CustomEvent('wa-select', { detail: { item }, bubbles: true, composed: true, cancelable: true }),
    );
  };

  // Top tabs squeezed by the padding hide the names of inactive tabs until
  // the bar is wider than where they ran out of room.
  const COMPACT = 'data-sanctum-compact';
  const COMPACT_STYLE = `:host([${COMPACT}]:not([active])) .name { display: none; }`;
  let compactWidth = 0;
  let compacted = [];
  const compactTabs = (bar, band) => {
    const tabs = [];
    if (bar) {
      eachDeep(bar, (node) => {
        if (node.localName === 'ha-tab' && !node.narrow && shows(node) && node.getBoundingClientRect().top < band) {
          tabs.push(node);
        }
      });
    }
    const width = bar?.getBoundingClientRect().width ?? 0;
    const squeezed = tabs.some((tab) => {
      const name = tab.shadowRoot?.querySelector('.name');
      return name && name.scrollWidth > name.clientWidth + 1;
    });
    if (squeezed && !compactWidth) compactWidth = width;
    else if (compactWidth && (!tabs.length || width > compactWidth + 24)) compactWidth = 0;
    for (const tab of compacted) {
      if (!compactWidth || !tabs.includes(tab)) tab.removeAttribute(COMPACT);
    }
    compacted = compactWidth ? tabs : [];
    for (const tab of compacted) {
      const root = tab.shadowRoot;
      if (root && !root.querySelector('style[data-sanctum]')) {
        const style = document.createElement('style');
        style.dataset.sanctum = '';
        style.textContent = COMPACT_STYLE;
        root.append(style);
      }
      tab.setAttribute(COMPACT, '');
    }
  };

  // Parts of a padded toolbar drawn below it, such as tabs that move to the
  // bottom on narrow windows, inherit its inset otherwise. They get the
  // page's value back.
  let shielded = new Set();
  const shieldOutside = (band) => {
    const outside = new Set();
    for (const bar of padded) {
      eachDeep(bar, (node) => {
        if (node === bar || !(node instanceof Element)) return;
        const box = node.getBoundingClientRect();
        if (box.height > 0 && box.top >= band - 1) outside.add(node);
      });
    }
    const root = getComputedStyle(document.documentElement);
    for (const node of outside) {
      for (const side of ['right', 'left']) {
        const value = root.getPropertyValue(`--safe-area-inset-${side}`).trim() || '0px';
        if (node.style.getPropertyValue(`--safe-area-inset-${side}`) !== value) {
          node.style.setProperty(`--safe-area-inset-${side}`, value);
        }
      }
    }
    for (const node of shielded) {
      if (!outside.has(node)) {
        node.style.removeProperty('--safe-area-inset-right');
        node.style.removeProperty('--safe-area-inset-left');
      }
    }
    shielded = outside;
  };

  // A scrollbar reaching the top of the window runs under Sanctum's window
  // buttons, so its track starts below the toolbar band. A track margin only
  // takes effect on a styled scrollbar, so these get the frontend's scrollbar
  // style.
  const SCROLL = 'data-sanctum-scroll';
  const SCROLL_STYLE =
    `[${SCROLL}]::-webkit-scrollbar { width: 0.4rem; height: 0.4rem; }` +
    `[${SCROLL}]::-webkit-scrollbar-thumb { border-radius: 4px; background: var(--scrollbar-thumb-color, rgba(0, 0, 0, 0.3)); }` +
    `[${SCROLL}]::-webkit-scrollbar-track { margin-top: var(--sanctum-scroll-inset, 0px); }`;
  let insetScrollers = new Set();
  const scrollable = (node) => {
    if (node === document.documentElement) {
      return node.scrollHeight > node.clientHeight;
    }
    const overflow = getComputedStyle(node).overflowY;
    return (overflow === 'auto' || overflow === 'scroll') && node.scrollHeight > node.clientHeight;
  };
  const scrollerAt = (x, y) => {
    let el = document.elementFromPoint(x, y);
    while (el?.shadowRoot) {
      const inner = el.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === el) break;
      el = inner;
    }
    for (let node = el; node; node = node.assignedSlot || node.parentNode || node.host) {
      if (node instanceof Element && node !== document.body && scrollable(node)) return node;
    }
    return scrollable(document.documentElement) ? document.documentElement : null;
  };
  const insetScrollbars = (band, active) => {
    const found = new Set();
    const scroller = active ? scrollerAt(innerWidth - 2, band + 4) : null;
    if (scroller) {
      const box = scroller.getBoundingClientRect();
      const top = scroller === document.documentElement ? 0 : box.top;
      if (box.right >= innerWidth - 2 && top < band) {
        const root = scroller.getRootNode();
        const host = root === document ? document.head : root;
        if (host && !host.querySelector?.(':scope > style[data-sanctum-scroll-style]')) {
          const style = document.createElement('style');
          style.dataset.sanctumScrollStyle = '';
          style.textContent = SCROLL_STYLE;
          host.append(style);
        }
        scroller.setAttribute(SCROLL, '');
        const inset = `${Math.ceil(band - top)}px`;
        if (scroller.style.getPropertyValue('--sanctum-scroll-inset') !== inset) {
          scroller.style.setProperty('--sanctum-scroll-inset', inset);
        }
        found.add(scroller);
      }
    }
    for (const node of insetScrollers) {
      if (!found.has(node)) {
        node.removeAttribute(SCROLL);
        node.style.removeProperty('--sanctum-scroll-inset');
      }
    }
    insetScrollers = found;
  };

  window.sanctumPadToolbars = () => {
    const insets = window.sanctumInsets ?? { start: 0, end: 0 };
    const band = length('--header-height', 56) + length('--safe-area-inset-top', 0);
    const wanted = new Map();
    for (const [side, value] of [['right', insets.end], ['left', insets.start]]) {
      if (!value) continue;
      const bar = side === 'right'
        ? barNear(innerWidth - value - 24, 24, band)
        : barNear(value + 24, innerWidth - 24, band);
      if (bar) wanted.set(bar, { ...wanted.get(bar), [side]: value });
    }
    // A dialog over the page hides its toolbar from the hit test, so a padded
    // toolbar keeps its padding while it is still in place under one.
    const covered = overlayAt(innerWidth - 88, band / 2);
    for (const bar of padded) {
      const kept =
        covered && (insets.start || insets.end) && bar.isConnected && inBand(bar, band);
      if (!wanted.has(bar) && !kept) {
        bar.style.removeProperty('--safe-area-inset-right');
        bar.style.removeProperty('--safe-area-inset-left');
        padded.delete(bar);
      }
    }
    for (const [bar, sides] of wanted) {
      for (const side of ['right', 'left']) {
        const value = sides[side] ? `${sides[side]}px` : '';
        if (bar.style.getPropertyValue(`--safe-area-inset-${side}`) !== value) {
          bar.style.setProperty(`--safe-area-inset-${side}`, value);
        }
      }
      padded.add(bar);
    }
    shieldOutside(band);
    if (window.sanctumTitlebar && !covered) {
      reportToolbar(wanted.keys().next().value ?? barNear(innerWidth - 88, 24, band));
    }
    insetScrollbars(band, !!insets.end);
    if (!covered) {
      const bar = insets.end ? wanted.keys().next().value : null;
      absorbMenu(bar, band);
      compactTabs(bar, band);
    }
  };
  setInterval(() => {
    if (padded.size || absorbed || absorbedButtons.length || window.sanctumTitlebar) {
      window.sanctumPadToolbars();
    }
  }, 400);

  // Sanctum's Default Dashboard entry is hidden only when the frontend shows
  // its default dashboard, worked out as the frontend does.
  let onDefault = false;
  const defaultPanel = (hass) => {
    let legacy = null;
    try {
      legacy = JSON.parse(localStorage.getItem('defaultPanel') ?? 'null');
    } catch {
      legacy = null;
    }
    const panel = hass.userData?.default_panel || hass.systemData?.default_panel || legacy || 'home';
    return panel === 'lovelace' && !hass.panels.lovelace?.config ? 'home' : panel;
  };

  // Some panels, such as Media, rewrite the address when the frontend goes
  // through "/" to find the default panel, so the panel's path is used.
  window.sanctumGoHome = () => {
    const hass = document.querySelector('home-assistant')?.hass;
    const path = hass?.panels ? `/${defaultPanel(hass)}` : '/';
    history.pushState(null, '', path);
    window.dispatchEvent(new CustomEvent('location-changed', { detail: { replace: false } }));
  };

  const reportDefault = () => {
    const hass = document.querySelector('home-assistant')?.hass;
    let showing = false;
    if (hass?.panels && hass.panelUrl) {
      showing = hass.panelUrl === defaultPanel(hass);
    }
    if (showing !== onDefault) {
      onDefault = showing;
      post({ type: 'default-dashboard', showing });
    }
  };
  setInterval(reportDefault, 400);

  const reportViewport = () => post({ type: 'viewport', width: innerWidth });
  addEventListener('resize', reportViewport);
  addEventListener('DOMContentLoaded', reportViewport);

  let pointer = 'mouse';
  let press = null;

  addEventListener('pointerdown', (event) => {
    pointer = event.pointerType;
    press = null;
    if (pointer === 'touch' || !onToolbar(event)) return;
    if (event.button === 0) {
      press = { x: event.screenX, y: event.screenY };
    } else if (event.button === 1) {
      post({ type: 'titlebar', action: 'middle' });
    }
  }, true);

  // A move starts only past the drag threshold, so clicks and double clicks
  // still reach the page.
  addEventListener('pointermove', (event) => {
    if (!press) return;
    if (!(event.buttons & 1)) {
      press = null;
      return;
    }
    if (Math.hypot(event.screenX - press.x, event.screenY - press.y) >= 8) {
      press = null;
      post({ type: 'titlebar', action: 'move' });
    }
  }, true);

  addEventListener('pointerup', () => { press = null; }, true);

  addEventListener('dblclick', (event) => {
    if (pointer === 'touch' || !onToolbar(event)) return;
    event.preventDefault();
    post({ type: 'titlebar', action: 'double' });
  }, true);

  addEventListener('contextmenu', (event) => {
    if (pointer === 'touch' || !onToolbar(event)) return;
    event.preventDefault();
    event.stopPropagation();
    post({ type: 'titlebar', action: 'right' });
  }, true);

  // Half the finger's travel, so about 200 px of pull.
  const PULL_DISTANCE = 100;
  // How long the pull stays past the threshold before a release counts.
  const PULL_HOLD = 150;
  const SIZE = 40;
  let pull = null;
  let indicator = null;

  // The arrow turns the primary color once a release would refresh.
  const showIndicator = (distance, armed, refreshing) => {
    if (!indicator) {
      indicator = document.createElement('div');
      indicator.setAttribute('aria-hidden', 'true');
      indicator.style.cssText =
        `position:fixed;top:0;left:50%;width:${SIZE}px;height:${SIZE}px;margin-left:-${SIZE / 2}px;` +
        'border-radius:50%;z-index:2147483647;pointer-events:none;display:flex;' +
        'align-items:center;justify-content:center;box-shadow:0 2px 6px rgba(0,0,0,0.3);' +
        'background:var(--card-background-color,#fff);color:var(--primary-color,#03a9f4);';
      indicator.innerHTML =
        '<svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor"><path d="M17.65,6.35' +
        'C16.2,4.9 14.21,4 12,4A8,8 0 0,0 4,12A8,8 0 0,0 12,20C15.73,20 18.84,17.45 19.73,14H17.65' +
        'C16.83,16.33 14.61,18 12,18A6,6 0 0,1 6,12A6,6 0 0,1 12,6C13.66,6 15.14,6.69 16.22,7.78' +
        'L13,11H20V4L17.65,6.35Z"/></svg>';
      document.documentElement.appendChild(indicator);
    }
    const top = Math.min(distance, PULL_DISTANCE * 1.5) - SIZE + length('--safe-area-inset-top', 0);
    indicator.style.transition = refreshing ? 'transform 0.2s' : 'none';
    indicator.style.opacity =
      armed || refreshing ? '1' : String(0.3 + (0.5 * Math.min(distance, PULL_DISTANCE)) / PULL_DISTANCE);
    indicator.style.color =
      armed || refreshing ? 'var(--primary-color,#03a9f4)' : 'var(--secondary-text-color,#727272)';
    indicator.style.transform = `translateY(${top}px) rotate(${distance * 3}deg)`;
    if (refreshing) {
      indicator.animate([{ rotate: '0deg' }, { rotate: '360deg' }], { duration: 800, iterations: Infinity });
    }
  };

  const hideIndicator = () => {
    clearTimeout(pull?.timer);
    indicator?.remove();
    indicator = null;
  };

  const scrolled = (path) =>
    (document.scrollingElement?.scrollTop ?? 0) > 0 ||
    path.some((node) => node instanceof Element && node.scrollTop > 0);

  addEventListener('touchstart', (event) => {
    pull = null;
    if (window.sanctumPullToRefresh === false || event.touches.length !== 1) return;
    const path = event.composedPath();
    if (scrolled(path)) return;
    if (path.some((node) => node instanceof Element && (node.matches(DIALOG) || node.matches(NO_PULL)))) return;
    const touch = event.touches[0];
    pull = { x: touch.clientX, y: touch.clientY, distance: 0, started: false };
  }, { passive: true, capture: true });

  // Listening last on the window lets maps and sliders that handle the
  // gesture mark it with preventDefault first.
  addEventListener('touchmove', (event) => {
    if (!pull) return;
    if (event.defaultPrevented || event.touches.length !== 1) {
      pull = null;
      hideIndicator();
      return;
    }
    const touch = event.touches[0];
    const dx = touch.clientX - pull.x;
    const dy = touch.clientY - pull.y;
    if (!pull.started) {
      if (Math.abs(dx) > Math.abs(dy) || dy < 0) {
        if (Math.abs(dx) > 10 || dy < -10) pull = null;
        return;
      }
      if (dy < 10) return;
      pull.started = true;
    }
    pull.distance = Math.max(0, dy) / 2;
    if (pull.distance < PULL_DISTANCE) {
      clearTimeout(pull.timer);
      pull.timer = null;
      pull.armed = false;
    } else if (!pull.armed && !pull.timer) {
      const current = pull;
      pull.timer = setTimeout(() => {
        current.armed = true;
        current.timer = null;
        if (pull === current) showIndicator(current.distance, true, false);
      }, PULL_HOLD);
    }
    showIndicator(pull.distance, pull.armed, false);
  }, { passive: true });

  addEventListener('touchend', () => {
    if (pull?.started && pull.armed) {
      showIndicator(PULL_DISTANCE, true, true);
      post({ type: 'refresh' });
    } else {
      hideIndicator();
    }
    pull = null;
  }, { passive: true });

  addEventListener('touchcancel', () => {
    pull = null;
    hideIndicator();
  }, { passive: true });
})();
