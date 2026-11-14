// How a Ziggy player goes together: a Raspberry Pi 3 A+ on the back of a 7" DSI display.
//
// Sizes are in millimetres, from the display's manual (Hosyond 7inch DSI Display V1.1) and the
// official Raspberry Pi 3 A+ mechanical drawing.
//
// Frames. The "device frame" is fixed to the display: x to the right and y up as seen from the
// back, z out of the back. "bx, by" are measured on the back from its lower-left corner. While
// you build, the display lies face down on the table, so the device's z points up at the room.
// The "Pi frame" is fixed to the Pi: x along the HDMI edge from the DISPLAY connector's end,
// y towards the GPIO header, z up from the underside of the board.
//
// Motion. One timeline T runs from 0 to 9; step n is finished at T = n. Every moving part has
// keyframes on that timeline, so going to a step plays everything between here and there.

import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { RoomEnvironment } from "three/addons/environments/RoomEnvironment.js";
import { RoundedBoxGeometry } from "three/addons/geometries/RoundedBoxGeometry.js";

const DISPLAY = { w: 164.9, h: 102, pcb: 1.6, body: 6.6, glass: 0.7 };
const TABLE = -(DISPLAY.pcb + DISPLAY.body + DISPLAY.glass); // the table top, in the device frame
// The Pi sits turned 180° so its GPIO header lies over the display's spring pins.
// Pi corner (0, 0) lands at bx = 121.86, by = 70.31.
const PI_ORIGIN = { bx: 121.86, by: 70.31 };
const STANDOFF = 4; // M2.5 × 4 + 4 copper standoffs
const PI = { w: 65, h: 56, pcb: 1.6, holes: [[3.5, 3.5], [61.5, 3.5], [3.5, 52.5], [61.5, 52.5]] };
const PI_TOP = PI.pcb;
const FPC = { bx: 125.16, by: 42.36 }; // the display's ribbon connector

const wx = (bx) => bx - DISPLAY.w / 2;
const wy = (by) => by - DISPLAY.h / 2;

// A camera position from where it looks, how far away, how high (degrees) and round to which side.
function view(target, distance, elevation, azimuth) {
  const el = THREE.MathUtils.degToRad(elevation);
  const az = THREE.MathUtils.degToRad(azimuth);
  return [
    [
      target[0] + distance * Math.sin(az) * Math.cos(el),
      target[1] + distance * Math.sin(el),
      target[2] + distance * Math.cos(az) * Math.cos(el),
    ],
    target,
  ];
}

export const STEPS = [
  {
    title: "The parts",
    text: "A Raspberry Pi 3 A+, a 7\" DSI touchscreen (it comes with four copper standoffs, four screws a short ribbon cable and two legs), a microSD card, a power supply, and if you like, a USB microphone for voice control. Lay the display face down on a soft cloth.",
    cam: view([30, 0, 5], 560, 50, 0),
  },
  {
    title: "Put the microSD card in the Pi",
    text: "Write Raspberry Pi OS Lite to a 16 GB card first. Turn the Pi over: the slot is on its underside, at the end with the DISPLAY connector. Push the card in with its contacts facing the board.",
    cam: view([154, 10, 8], 215, 56, 10),
  },
  {
    title: "Screw the standoffs into the display",
    text: "The four copper standoffs go into the four holes in the middle of the display's back, the ones that match the Pi's corners. Finger-tight is enough.",
    cam: view([4, 8, 6], 330, 46, -12),
  },
  {
    title: "Screw the Pi onto the standoffs",
    text: "Turn the Pi back over and lay it on the standoffs, components up, with the row of GPIO pins towards the bottom edge of the display, over its two gold spring pins. Four M2.5 screws hold it down. Tighten them until the spring pins press firmly on the Pi: they power the screen, so no extra wires are needed.",
    cam: view([10, 10, 4], 400, 46, 10),
    pace: 1.9,
  },
  {
    title: "Plug the ribbon into the display",
    text: "The display's connector is just beside the Pi. Pull its dark latch up gently, push one end of the ribbon in, and press the latch back down.",
    cam: view([30, 18, 8], 250, 34, 30),
  },
  {
    title: "Plug the ribbon into the Pi",
    text: "Lift the latch on the Pi's DISPLAY connector, fold the other end of the ribbon over and slide it in, then press the latch down.",
    cam: view([38, 22, 9], 150, 30, 42),
  },
  {
    title: "Optional: a USB microphone",
    text: "For voice control, plug a small USB microphone into the Pi's one USB port. Without one, everything else works the same.",
    cam: view([-36, 16, 22], 230, 34, -32),
  },
  {
    title: "Screw on the legs",
    text: "The two black legs go on the bottom corners of the display's back, each held by one of the long black screws. Their arms point away from the display.",
    cam: view([0, 12, 14], 400, 44, -6),
  },
  {
    title: "Plug in the power",
    text: "A 5 V micro-USB supply of 2.5 A goes into the Pi's power socket. A right-angle plug keeps the cable flat along the Pi, so it doesn't stick up above the screen. The Pi starts as soon as it has power.",
    cam: view([30, 14, -34], 300, 40, 58),
  },
  {
    title: "Stand it up",
    text: "Turn the display round and stand it on its legs. The Pi boots straight into Ziggy, and after you run ziggy setup from your computer, your library is on the screen.",
    cam: view([0, 46, 0], 400, 13, 26),
  },
];

// ---------- textures drawn on a canvas ----------

function canvasTexture(wmm, hmm, pxPerMm, draw) {
  const c = document.createElement("canvas");
  c.width = Math.round(wmm * pxPerMm);
  c.height = Math.round(hmm * pxPerMm);
  const g = c.getContext("2d");
  // Draw in millimetres with y up, like the board drawings.
  g.setTransform(pxPerMm, 0, 0, -pxPerMm, 0, c.height);
  draw(g);
  const t = new THREE.CanvasTexture(c);
  t.colorSpace = THREE.SRGBColorSpace;
  t.anisotropy = 8;
  return t;
}

function text(g, s, x, y, size, { rot = 0, align = "left", color = "#f2f2f2", weight = 600 } = {}) {
  g.save();
  g.translate(x, y);
  g.rotate(rot);
  g.scale(1, -1);
  g.fillStyle = color;
  g.font = `${weight} ${size}px "Helvetica Neue", Arial, sans-serif`;
  g.textAlign = align;
  g.textBaseline = "middle";
  g.fillText(s, 0, 0);
  g.restore();
}

function ring(g, x, y, r, inner) {
  g.fillStyle = "#d8b45a";
  g.beginPath();
  g.arc(x, y, r, 0, Math.PI * 2);
  g.fill();
  g.fillStyle = "#101010";
  g.beginPath();
  g.arc(x, y, inner, 0, Math.PI * 2);
  g.fill();
}

function seeded(seed) {
  let s = seed;
  return () => (s = (s * 16807) % 2147483647) / 2147483647;
}

// Copper tracks under the solder mask, with a pad at each end.
function traces(g, w, h, color, n, seed) {
  const rnd = seeded(seed);
  g.strokeStyle = color;
  g.fillStyle = color;
  g.lineWidth = 0.22;
  for (let i = 0; i < n; i++) {
    let x = rnd() * w;
    let y = rnd() * h;
    g.beginPath();
    g.moveTo(x, y);
    for (let k = 0; k < 4; k++) {
      const len = (rnd() - 0.5) * 26;
      if (k % 2 === 0) x += len;
      else y += len;
      g.lineTo(x, y);
    }
    g.stroke();
    g.beginPath();
    g.arc(x, y, 0.35, 0, Math.PI * 2);
    g.fill();
  }
}

function piTexture() {
  return canvasTexture(PI.w, PI.h, 16, (g) => {
    g.fillStyle = "#157a3b";
    g.fillRect(0, 0, PI.w, PI.h);
    traces(g, PI.w, PI.h, "rgba(150, 225, 160, 0.2)", 110, 7);
    for (const [x, y] of PI.holes) ring(g, x, y, 3, 1.4);
    text(g, "Raspberry Pi 3 Model A+", 20, 45.5, 2.4);
    text(g, "© Raspberry Pi 2018", 20, 42.6, 1.8, { weight: 400 });
    text(g, "HDMI", 32, 13.5, 2.6, { align: "center" });
    text(g, "DISPLAY", 7.2, 28, 1.8, { rot: Math.PI / 2, align: "center" });
    text(g, "CAMERA", 49.2, 12, 1.8, { rot: Math.PI / 2, align: "center" });
    text(g, "GPIO", 3.5, 47.5, 1.6);
    text(g, "PWR IN", 10.6, 6.8, 1.5, { align: "center" });
    text(g, "USB", 50.5, 31.5, 1.8, { rot: Math.PI / 2, align: "center" });
    text(g, "Made in the UK", 18, 40, 1.4, { weight: 400 });
    g.strokeStyle = "#f2f2f2";
    g.lineWidth = 0.35;
    for (const [dx, dy] of [[0, 0], [-1.6, 1.2], [1.6, 1.2], [0, 2.4], [-1.2, -1.6], [1.2, -1.6]]) {
      g.beginPath();
      g.arc(42 + dx, 43 + dy, 1.1, 0, Math.PI * 2);
      g.stroke();
    }
  });
}

function piUnderTexture() {
  return canvasTexture(PI.w, PI.h, 10, (g) => {
    g.fillStyle = "#157a3b";
    g.fillRect(0, 0, PI.w, PI.h);
    traces(g, PI.w, PI.h, "rgba(150, 225, 160, 0.2)", 90, 11);
    for (const [x, y] of PI.holes) ring(g, x, y, 3, 1.4);
    // The header's solder joints.
    g.fillStyle = "#c9ccd0";
    for (let k = 0; k < 20; k++) {
      for (const y of [51.23, 53.77]) {
        g.beginPath();
        g.arc(8.37 + 2.54 * k, y, 0.75, 0, Math.PI * 2);
        g.fill();
      }
    }
  });
}

function displayBackTexture() {
  return canvasTexture(DISPLAY.w, DISPLAY.h, 8, (g) => {
    g.fillStyle = "#1c5aa8";
    g.fillRect(0, 0, DISPLAY.w, DISPLAY.h);
    traces(g, DISPLAY.w, DISPLAY.h, "rgba(160, 205, 255, 0.16)", 170, 3);
    for (const x of [5, DISPLAY.w - 5]) for (const y of [5, DISPLAY.h - 5]) ring(g, x, y, 3.2, 1.4);
    for (const [lx, ly] of PI.holes) ring(g, PI_ORIGIN.bx - lx, PI_ORIGIN.by - ly, 3, 1.25);
    text(g, "7inch DSI Display", 60, 12, 4.2, { weight: 700 });
    text(g, "800X480 Pixel", 60, 7, 2.6, { weight: 400 });
    text(g, "CE  RoHS", 104, 9, 2.8, { weight: 700 });
    text(g, "Backlight", 11, 90, 2.2, { weight: 400 });
    text(g, "+5V", 131, 7, 1.8, { weight: 400 });
    text(g, "Be Careful: push and pull the latch on both sides", 140, 50, 1.4, {
      rot: -Math.PI / 2,
      align: "center",
      weight: 400,
    });
  });
}

function matTexture() {
  const t = canvasTexture(100, 100, 5.12, (g) => {
    g.fillStyle = "#24212f";
    g.fillRect(0, 0, 100, 100);
    g.strokeStyle = "rgba(255, 255, 255, 0.05)";
    g.lineWidth = 0.3;
    for (let i = 0; i <= 100; i += 10) {
      g.beginPath();
      g.moveTo(i, 0);
      g.lineTo(i, 100);
      g.moveTo(0, i);
      g.lineTo(100, i);
      g.stroke();
    }
  });
  t.wrapS = t.wrapT = THREE.RepeatWrapping;
  t.repeat.set(40, 40);
  return t;
}

// ---------- building blocks ----------

const std = (o) => new THREE.MeshStandardMaterial(o);
const M = {
  silver: std({ color: 0xd4d8dd, metalness: 0.95, roughness: 0.3 }),
  steel: std({ color: 0x8f969d, metalness: 0.9, roughness: 0.38 }),
  black: std({ color: 0x17171a, roughness: 0.6 }),
  chip: std({ color: 0x1d1d21, roughness: 0.35 }),
  white: std({ color: 0xecebe6, roughness: 0.6 }),
  latch: std({ color: 0x4b3628, roughness: 0.6 }),
  gold: std({ color: 0xe1b350, metalness: 1, roughness: 0.25 }),
  copper: std({ color: 0xd49a52, metalness: 1, roughness: 0.32 }),
  green: std({ color: 0x115f2f, roughness: 0.6 }),
  blue: std({ color: 0x184c8c, roughness: 0.6 }),
  glass: std({ color: 0x050507, metalness: 0.2, roughness: 0.08 }),
  ribbon: std({ color: 0xdcd9cf, roughness: 0.45, side: THREE.DoubleSide }),
  stiffener: std({ color: 0x2b66c4, roughness: 0.5 }),
  sd: std({ color: 0x1f1f25, roughness: 0.45 }),
  cable: std({ color: 0x1b1b1f, roughness: 0.55 }),
  cap: std({ color: 0xb59d77, roughness: 0.6 }),
  flex: std({ color: 0xc98a2e, roughness: 0.45, metalness: 0.2 }),
  dark: new THREE.MeshBasicMaterial({ color: 0x050505 }),
};

// A box given by its min corner and size, in the parent's millimetres, with softened edges.
function box(parent, mat, [x, y, z], [w, h, d], radius = 0.25) {
  const r = Math.min(radius, Math.min(w, h, d) / 2.2);
  const geo = r > 0.04 ? new RoundedBoxGeometry(w, h, d, 2, r) : new THREE.BoxGeometry(w, h, d);
  const m = new THREE.Mesh(geo, mat);
  m.position.set(x + w / 2, y + h / 2, z + d / 2);
  parent.add(m);
  return m;
}

// A cylinder standing along z.
function post(parent, mat, [x, y, z], radius, height, sides = 24) {
  const m = new THREE.Mesh(new THREE.CylinderGeometry(radius, radius, height, sides), mat);
  m.rotation.x = Math.PI / 2;
  m.position.set(x, y, z + height / 2);
  parent.add(m);
  return m;
}

function roundedRect(w, h, r) {
  const s = new THREE.Shape();
  s.moveTo(r, 0);
  s.lineTo(w - r, 0);
  s.absarc(w - r, r, r, -Math.PI / 2, 0);
  s.lineTo(w, h - r);
  s.absarc(w - r, h - r, r, 0, Math.PI / 2);
  s.lineTo(r, h);
  s.absarc(r, h - r, r, Math.PI / 2, Math.PI);
  s.lineTo(0, r);
  s.absarc(r, r, r, Math.PI, Math.PI * 1.5);
  return s;
}

// Small capacitors and resistors, wherever nothing bigger is in the way.
function sprinkle(parent, z, [x0, y0, x1, y1], keepOut, count, seed) {
  const rnd = seeded(seed);
  const kinds = [
    [M.cap, 1.0, 0.5, 0.5],
    [M.cap, 1.6, 0.8, 0.7],
    [M.black, 1.6, 0.8, 0.45],
    [M.chip, 3, 2, 0.9],
  ];
  let placed = 0;
  for (let tries = 0; placed < count && tries < count * 30; tries++) {
    const [mat, a, b, h] = kinds[Math.floor(rnd() * kinds.length)];
    const turned = rnd() > 0.5;
    const w = turned ? b : a;
    const d = turned ? a : b;
    const x = x0 + rnd() * (x1 - x0 - w);
    const y = y0 + rnd() * (y1 - y0 - d);
    if (keepOut.some(([kx0, ky0, kx1, ky1]) => x < kx1 && x + w > kx0 && y < ky1 && y + d > ky0)) continue;
    keepOut.push([x - 0.4, y - 0.4, x + w + 0.4, y + d + 0.4]);
    box(parent, mat, [x, y, z], [w, d, h], 0.08);
    placed++;
  }
}

// ---------- the display ----------

function buildDisplay(screenTexture) {
  const g = new THREE.Group();
  const backMat = std({ map: displayBackTexture(), roughness: 0.55 });
  const pcb = new THREE.Mesh(
    new THREE.BoxGeometry(DISPLAY.w, DISPLAY.h, DISPLAY.pcb),
    [M.blue, M.blue, M.blue, M.blue, backMat, M.blue],
  );
  pcb.position.z = -DISPLAY.pcb / 2;
  g.add(pcb);
  // The panel: a thin metal frame, black glass on the front.
  box(g, M.steel, [-82.19, -49.585, -DISPLAY.pcb - DISPLAY.body], [164.38, 99.17, DISPLAY.body], 0.5);
  box(g, M.glass, [-82.19, -49.585, TABLE], [164.38, 99.17, DISPLAY.glass], 0.3);
  // The picture: facing −z, turned so it reads correctly from the front.
  const screenMat = new THREE.MeshBasicMaterial({ map: screenTexture, color: 0x000000, toneMapped: false });
  const screen = new THREE.Mesh(new THREE.PlaneGeometry(154.68, 87.02), screenMat);
  screen.rotation.y = Math.PI;
  screen.position.set(0, 1.5, TABLE - 0.05);
  g.add(screen);

  // Ribbon connector: 15 pins at 1 mm. Its latch is a separate part, so it can open.
  box(g, M.black, [wx(FPC.bx) - 2, wy(FPC.by) - 9.5, 0], [4, 19, 2.2]);
  // The panel's own flex and its connector.
  box(g, M.flex, [wx(70), wy(42), 0], [26, 17, 0.25], 0.05);
  box(g, M.white, [wx(69), wy(59), 0], [28, 3.2, 1.6]);
  box(g, M.latch, [wx(69), wy(62), 0], [28, 1.4, 1.3]);
  // Spring pins under the Pi's 5 V and GND pins (4 and 6), and the 5 V socket.
  for (const lx of [10.91, 13.45]) {
    post(g, M.gold, [wx(PI_ORIGIN.bx - lx), wy(PI_ORIGIN.by - 53.77), 0], 0.9, 1.2, 16);
    post(g, M.gold, [wx(PI_ORIGIN.bx - lx), wy(PI_ORIGIN.by - 53.77), 1.2], 0.5, 1.3, 16);
  }
  box(g, M.white, [wx(124), wy(9), 0], [5, 7.5, 5.8]);
  // Backlight button, chips, an inductor.
  box(g, M.steel, [wx(15.5), wy(93), 0], [4.5, 3.2, 1.4]);
  post(g, M.black, [wx(17.75), wy(94.6), 1.4], 0.9, 0.5);
  box(g, M.chip, [wx(28), wy(62), 0], [7, 7, 1], 0.1);
  box(g, M.chip, [wx(100), wy(74), 0], [6, 6, 1], 0.1);
  box(g, M.chip, [wx(40), wy(30), 0], [5, 4, 0.9], 0.1);
  box(g, M.black, [wx(104), wy(58), 0], [6, 6, 3], 0.8);
  const keepOut = [
    [67, 40, 99, 65], [120, 30, 130, 55], [122, 7, 136, 18], [56, 2, 124, 16], [104, 12, 115, 21],
    [26, 60, 37, 71], [98, 72, 108, 82], [38, 28, 47, 36], [102, 56, 112, 66], [136, 10, 144, 92],
    ...PI.holes.map(([lx, ly]) => [PI_ORIGIN.bx - lx - 4, PI_ORIGIN.by - ly - 4, PI_ORIGIN.bx - lx + 4, PI_ORIGIN.by - ly + 4]),
  ];
  const parts = new THREE.Group();
  parts.position.set(-DISPLAY.w / 2, -DISPLAY.h / 2, 0);
  g.add(parts);
  sprinkle(parts, 0, [12, 18, 134, 88], keepOut, 70, 21);
  return { group: g, screenMat };
}

// ---------- the Raspberry Pi 3 A+, in the Pi frame ----------

function buildPi() {
  const g = new THREE.Group();
  const shape = roundedRect(PI.w, PI.h, 3);
  for (const [x, y] of PI.holes) {
    const hole = new THREE.Path();
    hole.absarc(x, y, 1.375, 0, Math.PI * 2, true);
    shape.holes.push(hole);
  }
  // The board itself carries the plain underside on both faces; the printed top is its own
  // face just above, so the printing never shows through mirrored underneath.
  const under = piUnderTexture();
  under.repeat.set(1 / PI.w, 1 / PI.h);
  g.add(new THREE.Mesh(
    new THREE.ExtrudeGeometry(shape, { depth: PI.pcb, bevelEnabled: false, curveSegments: 24 }),
    [std({ map: under, roughness: 0.6 }), M.green],
  ));
  const tex = piTexture();
  tex.repeat.set(1 / PI.w, 1 / PI.h);
  const face = new THREE.Mesh(new THREE.ShapeGeometry(shape, 24), std({ map: tex, roughness: 0.5 }));
  face.position.z = PI.pcb + 0.01;
  g.add(face);
  const top = PI_TOP;

  // GPIO header: black base, 40 gold pins that also poke through underneath.
  box(g, M.black, [7.2, 50, top], [51, 5, 2.5], 0.15);
  const pin = new THREE.BoxGeometry(0.64, 0.64, 10.2);
  for (let k = 0; k < 20; k++) {
    for (const y of [51.23, 53.77]) {
      const p = new THREE.Mesh(pin, M.gold);
      p.position.set(8.37 + 2.54 * k, y, top + 8.5 - 5.1 - 1.6);
      g.add(p);
    }
  }
  // Power socket.
  box(g, M.silver, [6.85, -1, top], [7.5, 5.5, 2.8], 0.5);
  box(g, M.dark, [7.6, -1.06, top + 0.6], [6, 0.2, 1.6], 0);
  // HDMI.
  box(g, M.silver, [24.75, -1.5, top], [14.5, 12, 6.2], 0.6);
  box(g, M.dark, [26, -1.56, top + 1], [12, 0.2, 4.2], 0);
  box(g, M.black, [27.5, -1.2, top + 2.6], [9, 3, 1], 0.1);
  // Audio jack.
  box(g, M.black, [50, 0, top], [7, 12.5, 6], 0.4);
  const jack = new THREE.Mesh(new THREE.CylinderGeometry(3, 3, 2.5, 28), M.black);
  jack.position.set(53.5, -1.25, top + 3);
  g.add(jack);
  const jackHole = new THREE.Mesh(new THREE.CylinderGeometry(1.75, 1.75, 0.3, 24), M.dark);
  jackHole.position.set(53.5, -2.55, top + 3);
  g.add(jackHole);
  // Camera connector, and the body of the display connector (its latch is a separate part).
  box(g, M.black, [43, 1, top], [4, 22, 5.5]);
  box(g, M.white, [43.3, 1, top + 5.5], [3.4, 22, 0.8], 0.1);
  box(g, M.black, [2.5, 17, top], [3, 22, 5.5]);
  // One USB-A port on the end.
  box(g, M.silver, [53.5, 25, top], [14, 13, 7], 0.5);
  box(g, M.dark, [67.46, 26, top + 0.9], [0.15, 11, 5.2], 0);
  box(g, std({ color: 0x242424, roughness: 0.5 }), [58, 26.8, top + 3.7], [9, 9.4, 1.6], 0.1);
  // Processor under a metal lid, the wireless can, smaller chips, LEDs.
  box(g, M.silver, [20, 24.6, top], [14, 14, 1.2], 0.3);
  const lid = canvasTexture(12, 12, 24, (c) => {
    c.fillStyle = "#c9cdd2";
    c.fillRect(0, 0, 12, 12);
    text(c, "BROADCOM", 6, 7.6, 1.5, { align: "center", color: "#6d7278", weight: 700 });
    text(c, "BCM2837B0", 6, 5.4, 1.3, { align: "center", color: "#6d7278", weight: 500 });
  });
  const lidTop = new THREE.Mesh(new THREE.PlaneGeometry(12, 12), std({ map: lid, metalness: 0.85, roughness: 0.35 }));
  lidTop.position.set(27, 31.6, top + 1.21);
  g.add(lidTop);
  box(g, M.silver, [7, 38, top], [10, 11, 1.6], 0.3);
  box(g, M.chip, [12, 10, top], [5, 5, 0.9], 0.1);
  box(g, M.chip, [36, 41, top], [5, 4, 0.9], 0.1);
  box(g, M.black, [16, 16, top], [4, 4, 2], 0.5);
  box(g, std({ color: 0xff3b30, emissive: 0x551010 }), [0.8, 8, top], [1.2, 1.6, 0.6], 0.1);
  box(g, std({ color: 0x34c759, emissive: 0x0d4419 }), [0.8, 10.5, top], [1.2, 1.6, 0.6], 0.1);
  sprinkle(g, top, [1, 1, 64, 49], [
    [6, -1, 15.5, 6], [24, -2, 40, 15.5], [49, -1, 58, 13.5], [42, 0, 51.5, 24], [1.5, 16, 9.5, 40],
    [48, 24, 66, 39], [19, 23.5, 35, 39.5], [6, 37, 18, 50], [11, 9, 18, 16], [35, 40, 46, 47.5],
    [15, 15, 21, 21], [0, 7, 3, 13], [17, 38.5, 44, 47.5],
    ...PI.holes.map(([x, y]) => [x - 4, y - 4, x + 4, y + 4]),
  ], 46, 5);
  // Underneath: the RAM, and the microSD holder at the DISPLAY end.
  box(g, M.chip, [20, 24.6, -1], [14, 14, 1], 0.1);
  box(g, M.steel, [6.5, 20.5, -1.5], [7.2, 15, 0.3], 0.05);
  box(g, M.steel, [0.2, 20.5, -1.5], [13.5, 1.6, 1.5], 0.05);
  box(g, M.steel, [0.2, 33.9, -1.5], [13.5, 1.6, 1.5], 0.05);
  box(g, M.steel, [12.6, 20.5, -1.5], [1.1, 15, 1.5], 0.05);
  return g;
}

function buildSdCard() {
  const g = new THREE.Group();
  const s = new THREE.Shape();
  // 15 × 11 mm with the stepped corner.
  s.moveTo(0, 0);
  s.lineTo(15, 0);
  s.lineTo(15, 11);
  s.lineTo(1.5, 11);
  s.lineTo(1.5, 9);
  s.lineTo(0, 8);
  s.lineTo(0, 0);
  g.add(new THREE.Mesh(new THREE.ExtrudeGeometry(s, { depth: 0.8, bevelEnabled: false }), M.sd));
  const label = canvasTexture(12, 10, 24, (c) => {
    c.fillStyle = "#c0262d";
    c.fillRect(0, 0, 12, 10);
    text(c, "16GB", 6, 6, 3.2, { align: "center", weight: 800 });
    text(c, "microSD", 6, 2.6, 1.8, { align: "center", weight: 500 });
  });
  const face = new THREE.Mesh(new THREE.PlaneGeometry(12, 10), std({ map: label, roughness: 0.5 }));
  face.rotation.x = Math.PI; // the label faces away from the board
  face.position.set(8.8, 5.5, -0.01);
  g.add(face);
  return g;
}

// An M2.5 screw: the head sits on z = 0, the thread hangs below.
function buildScrew(length = 5, mat = M.steel) {
  const g = new THREE.Group();
  post(g, mat, [0, 0, 0], 2.25, 1.5, 28);
  post(g, mat, [0, 0, -length], 1.2, length, 16);
  box(g, M.dark, [-1.5, -0.25, 1.35], [3, 0.5, 0.2], 0);
  box(g, M.dark, [-0.25, -1.5, 1.35], [0.5, 3, 0.2], 0);
  return g;
}

// A copper standoff: a hexagon 4 mm tall on z = 0, with its thread below.
function buildStandoff() {
  const g = new THREE.Group();
  post(g, M.copper, [0, 0, 0], 2.6, STANDOFF, 6);
  post(g, M.copper, [0, 0, -4], 1.2, 4, 16);
  post(g, M.dark, [0, 0, STANDOFF - 0.05], 1.05, 0.08, 16);
  return g;
}

// A small USB microphone. Its plug points along −x; the grille on top covers the microphone.
function buildMicrophone() {
  const g = new THREE.Group();
  box(g, M.silver, [-11, -6, -2.25], [11, 12, 4.5], 0.3);
  box(g, M.black, [0, -8, -3.6], [21, 16, 7.2], 1.6);
  post(g, M.steel, [13.5, 0, 3.55], 4.2, 0.12, 32);
  for (const [dx, dy] of [[0, 0], [-2, 0], [2, 0], [0, -2], [0, 2], [-1.5, -1.5], [1.5, 1.5], [-1.5, 1.5], [1.5, -1.5]]) {
    post(g, M.dark, [13.5 + dx, dy, 3.66], 0.45, 0.03, 10);
  }
  return g;
}

// One of the display's two legs: a block that screws to a bottom corner of the back, and an arm
// that reaches out behind. Built around its screw hole at the origin, on z = 0.
const LEG = { block: 7, lean: 18, reach: 40 };
function buildLeg() {
  const g = new THREE.Group();
  const plastic = std({ color: 0x151517, roughness: 0.5 });
  box(g, plastic, [-4.5, -5, 0], [9, 11, LEG.block], 0.6);
  post(g, M.dark, [0, 0, LEG.block - 0.02], 1.5, 0.06, 16);
  // The arm ends where the table is once the display leans back on it.
  const lean = THREE.MathUtils.degToRad(LEG.lean);
  const floor = -(DISPLAY.h / 2) * Math.cos(lean) + TABLE * Math.sin(lean); // y·cos − z·sin on the table
  const tipY = (floor + LEG.reach * Math.sin(lean)) / Math.cos(lean) - LEG_HOLE_Y;
  const from = new THREE.Vector3(0, 0, LEG.block / 2);
  const to = new THREE.Vector3(0, tipY, LEG.reach);
  const arm = new THREE.Mesh(new RoundedBoxGeometry(9, 7, from.distanceTo(to) + 4, 2, 1.2), plastic);
  arm.position.copy(from).lerp(to, 0.5);
  arm.quaternion.setFromUnitVectors(new THREE.Vector3(0, 0, 1), to.clone().sub(from).normalize());
  g.add(arm);
  return g;
}
const LEG_HOLE_Y = wy(5);

// The power plug, a right-angle one: its tip points along +y and its body turns to run along
// −x, so the cable lies along the Pi's edge. The cable leaves from PLUG_CABLE.
const PLUG_CABLE = [-27, -5, 0];
function buildPowerPlug() {
  const g = new THREE.Group();
  box(g, M.silver, [-3.4, 0, -0.9], [6.8, 5.5, 1.8], 0.4);
  box(g, M.cable, [-5.5, -9.5, -3.5], [11, 9.5, 7], 1.4);
  box(g, M.cable, [-21, -9.5, -3.3], [17, 9, 6.6], 1.4);
  box(g, M.cable, [-27, -7.4, -2.4], [7, 4.8, 4.8], 1.2);
  return g;
}

// ---------- the ribbon ----------
// A flat strip 17 mm wide. It is kept as a start point and the angle of each short segment, so
// it can lie flat, stand up or fold over without ever changing length.

const RIBBON_N = 60;

function ribbonFromCurve(pts) {
  const curve = new THREE.CatmullRomCurve3(pts.map(([x, z]) => new THREE.Vector3(x, 0, z)));
  const p = curve.getSpacedPoints(RIBBON_N);
  const angles = [];
  let last = Math.PI / 2;
  let length = 0;
  for (let i = 0; i < RIBBON_N; i++) {
    let a = Math.atan2(p[i + 1].z - p[i].z, p[i + 1].x - p[i].x);
    while (a - last > Math.PI) a -= Math.PI * 2;
    while (a - last < -Math.PI) a += Math.PI * 2;
    angles.push(a);
    last = a;
    length += p[i].distanceTo(p[i + 1]);
  }
  return { angles, length };
}

function buildRibbon() {
  const x0 = wx(FPC.bx);
  const x1 = wx(PI_ORIGIN.bx - 4);
  const yc = wy(FPC.by);
  const folded = ribbonFromCurve([
    [x0, 1], [x0 + 0.2, 6], [x0 + 1.5, 12], [x0 + 1.5, 18], [x0 - 1, 23], [x0 - 5, 24.5],
    [x1 + 1, 22], [x1, 18], [x1, 13], [x1, 8.2],
  ]);
  const above = ribbonFromCurve([
    [x0, 1], [x0 + 0.3, 6], [x0 + 2.2, 13], [x0 + 2, 20], [x0 - 1, 26], [x0 - 5.5, 28],
    [x1 + 0.6, 26], [x1, 22], [x1, 17.5], [x1, 13],
  ]);
  const seg = folded.length / RIBBON_N;
  const flat = Array(RIBBON_N).fill(0);
  const up = Array(RIBBON_N).fill(Math.PI / 2);
  const lean = up.map((a, i) => a - 0.34 * Math.pow(i / RIBBON_N, 1.5));
  const tableY = -80;
  const keys = [
    { t: 3.05, x: -52, z: TABLE + 0.2, y: tableY, angles: flat },
    { t: 3.5, x: x0, z: 12, y: yc, angles: up },
    { t: 3.62, x: x0, z: 7, y: yc, angles: up },
    { t: 3.8, x: x0, z: 1, y: yc, angles: up },
    { t: 4, x: x0, z: 1, y: yc, angles: lean },
    { t: 4.2, x: x0, z: 1, y: yc, angles: lean },
    { t: 4.7, x: x0, z: 1, y: yc, angles: above.angles },
    { t: 4.86, x: x0, z: 1, y: yc, angles: folded.angles },
  ];

  const width = 17;
  const geo = new THREE.BufferGeometry();
  const pos = new Float32Array((RIBBON_N + 1) * 2 * 3);
  const idx = [];
  for (let i = 0; i < RIBBON_N; i++) {
    const a = i * 2;
    idx.push(a, a + 1, a + 2, a + 1, a + 3, a + 2);
  }
  geo.setAttribute("position", new THREE.BufferAttribute(pos, 3));
  geo.setIndex(idx);
  const mesh = new THREE.Mesh(geo, M.ribbon);
  mesh.frustumCulled = false;
  const tabGeo = new THREE.BoxGeometry(1, width + 0.2, 0.45);
  const tabs = [0, 1].map(() => {
    const t = new THREE.Mesh(tabGeo, M.stiffener);
    mesh.add(t);
    return t;
  });
  const px = new Float32Array(RIBBON_N + 1);
  const pz = new Float32Array(RIBBON_N + 1);

  mesh.userData.update = (T) => {
    let k = 0;
    while (k < keys.length - 2 && T >= keys[k + 1].t) k++;
    const a = keys[k];
    const b = keys[k + 1];
    const u = ease(clamp01((T - a.t) / (b.t - a.t)));
    const mix = (p, q) => p + (q - p) * u;
    const y = mix(a.y, b.y);
    // A lift on the way from the table, so the ribbon doesn't slide through the display.
    const hop = a.y !== b.y ? Math.sin(Math.PI * u) * 22 : 0;
    px[0] = mix(a.x, b.x);
    pz[0] = mix(a.z, b.z) + hop;
    for (let i = 0; i < RIBBON_N; i++) {
      const ang = mix(a.angles[i], b.angles[i]);
      px[i + 1] = px[i] + seg * Math.cos(ang);
      pz[i + 1] = pz[i] + seg * Math.sin(ang);
    }
    for (let i = 0; i <= RIBBON_N; i++) {
      for (const [side, dy] of [[0, -width / 2], [1, width / 2]]) {
        const o = (i * 2 + side) * 3;
        pos[o] = px[i];
        pos[o + 1] = y + dy;
        pos[o + 2] = pz[i];
      }
    }
    geo.attributes.position.needsUpdate = true;
    geo.computeVertexNormals();
    // Blue stiffeners on the last 5 mm of each end.
    const n = Math.round(5 / seg);
    for (const [tab, i, j] of [[tabs[0], 0, n], [tabs[1], RIBBON_N - n, RIBBON_N]]) {
      const dx = px[j] - px[i];
      const dz = pz[j] - pz[i];
      tab.scale.x = Math.hypot(dx, dz);
      tab.position.set((px[i] + px[j]) / 2, y, (pz[i] + pz[j]) / 2);
      tab.rotation.set(0, -Math.atan2(dz, dx), 0);
      tab.translateZ(0.3);
    }
  };
  mesh.userData.update(0);
  return mesh;
}

// ---------- motion ----------

const ease = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
const clamp01 = (t) => Math.min(1, Math.max(0, t));
const Q = (x, y, z, deg) => new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(x, y, z), THREE.MathUtils.degToRad(deg));
const Z180 = Q(0, 0, 1, 180);
const IDENT = new THREE.Quaternion();

// A keyframe: where a part is at time t. `frame` says what the position is measured from:
// the display ("dev") or the Pi ("pi"), so a part can ride along with the Pi. `turns` is how
// many times it has been turned about its own axis, for screws. `lift` makes the move up to
// this keyframe an arc that high, like a hand carrying the part.
const key = (t, frame, p, q = IDENT, o = {}) => ({ t, frame, p: new THREE.Vector3(...p), q, turns: 0, lift: 0, linear: false, ...o });

const _pa = new THREE.Vector3();
const _pb = new THREE.Vector3();
const _qa = new THREE.Quaternion();
const _qb = new THREE.Quaternion();
const _spin = new THREE.Quaternion();
const _axisZ = new THREE.Vector3(0, 0, 1);

function resolve(k, pi, p, q) {
  p.copy(k.p);
  q.copy(k.q);
  if (k.frame === "pi") {
    p.applyQuaternion(pi.quaternion).add(pi.position);
    q.premultiply(pi.quaternion);
  }
}

function place(obj, keys, T, pi) {
  let i = 0;
  while (i < keys.length - 1 && T >= keys[i + 1].t) i++;
  const a = keys[i];
  const b = keys[Math.min(i + 1, keys.length - 1)];
  resolve(a, pi, _pa, _qa);
  if (a === b || T <= a.t) {
    obj.position.copy(_pa);
    obj.quaternion.copy(_qa);
    _spin.setFromAxisAngle(_axisZ, a.turns * Math.PI * 2);
  } else {
    const raw = clamp01((T - a.t) / (b.t - a.t));
    const u = b.linear ? raw : ease(raw);
    resolve(b, pi, _pb, _qb);
    obj.position.lerpVectors(_pa, _pb, u);
    obj.position.z += Math.sin(Math.PI * u) * b.lift;
    obj.quaternion.slerpQuaternions(_qa, _qb, u);
    _spin.setFromAxisAngle(_axisZ, (a.turns + (b.turns - a.turns) * u) * Math.PI * 2);
  }
  obj.quaternion.multiply(_spin);
}

// ---------- the scene ----------

// `finished: true` shows only the assembled player standing on its legs, on a see-through
// background, swaying slowly: the picture at the top of the page.
export function mountAssembly(container, { screenUrl, onStep, finished = false }) {
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: finished });
  renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.05;
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  container.appendChild(renderer.domElement);

  const BG = 0x17151f;
  const scene = new THREE.Scene();
  if (!finished) {
    scene.background = new THREE.Color(BG);
    scene.fog = new THREE.Fog(BG, 620, 1500);
  }
  const pmrem = new THREE.PMREMGenerator(renderer);
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  const sun = new THREE.DirectionalLight(0xfff4e6, 2.2);
  sun.position.set(-170, 420, 230);
  sun.castShadow = true;
  sun.shadow.mapSize.set(2048, 2048);
  Object.assign(sun.shadow.camera, { left: -330, right: 330, top: 330, bottom: -330, near: 50, far: 1100 });
  sun.shadow.bias = -0.0004;
  sun.shadow.normalBias = 0.5;
  sun.shadow.radius = 5;
  scene.add(sun, new THREE.HemisphereLight(0xdfe6ff, 0x2a2533, 0.5));

  const table = new THREE.Mesh(
    new THREE.PlaneGeometry(4000, 4000),
    // The finished picture has no mat, only the player's shadow on the page.
    finished ? new THREE.ShadowMaterial({ opacity: 0.45 }) : std({ map: matTexture(), roughness: 0.92, envMapIntensity: 0.25 }),
  );
  table.rotation.x = -Math.PI / 2;
  table.receiveShadow = true;
  scene.add(table);

  const camera = new THREE.PerspectiveCamera(32, 1, 5, 4000);
  const controls = new OrbitControls(camera, renderer.domElement);
  controls.enableDamping = true;
  controls.enablePan = false;
  controls.minDistance = 70;
  controls.maxDistance = 800;
  controls.maxPolarAngle = Math.PI / 2 - 0.04; // not under the table
  controls.enableZoom = !finished; // the wheel scrolls the page there
  let touched = false;
  controls.addEventListener("start", () => (touched = true));
  if (finished && matchMedia("(pointer: coarse)").matches) {
    // On a phone a finger on the picture should scroll the page, not turn the player.
    controls.enabled = false;
    renderer.domElement.style.touchAction = "auto";
  }

  const screenTexture = new THREE.TextureLoader().load(screenUrl);
  screenTexture.colorSpace = THREE.SRGBColorSpace;
  screenTexture.anisotropy = 8;

  // The device: the display and everything that ends up on it.
  const device = new THREE.Group();
  scene.add(device);
  const display = buildDisplay(screenTexture);
  device.add(display.group);

  // Lying face down, then stood upright with its back to you, then turned round and leant back.
  const lying = Q(1, 0, 0, -90);
  const lean = THREE.MathUtils.degToRad(LEG.lean);
  const standing = (DISPLAY.h / 2) * Math.cos(lean) - TABLE * Math.sin(lean); // height of the middle
  const deviceKeys = [
    key(8.08, "dev", [0, -TABLE, 0], lying),
    key(8.5, "dev", [0, 80, 0], IDENT),
    key(8.96, "dev", [0, standing, 0], Q(1, 0, 0, -LEG.lean).multiply(Q(0, 1, 0, 180))),
  ];

  const tracks = []; // parts placed in the device frame, some riding on the Pi
  const track = (obj, keys) => {
    device.add(obj);
    tracks.push({ obj, keys });
    return obj;
  };

  // The Pi: on the table to the right, turned over for the card, then onto the standoffs.
  const pi = buildPi();
  device.add(pi);
  const piRest = [172.5, 23, TABLE + 1.7];
  const piSeat = [wx(PI_ORIGIN.bx), wy(PI_ORIGIN.by), STANDOFF];
  // Turned over, it rests on its header pins with the underside and the card slot facing up.
  // It stays that way, so the card can be seen, until it goes onto the standoffs.
  const over = Q(1, 0, 0, 180).multiply(Z180);
  const piOver = new THREE.Vector3(140, -5, TABLE + 8.5 + 0.8).sub(new THREE.Vector3(32.5, 28, 0.8).applyQuaternion(over)).toArray();
  const piKeys = [
    key(0.04, "dev", piRest, Z180),
    key(0.36, "dev", piOver, over, { lift: 34 }),
    key(2.05, "dev", piOver, over),
    key(2.32, "dev", [piSeat[0], piSeat[1], STANDOFF + 20], Z180, { lift: 22 }),
    key(2.48, "dev", piSeat, Z180),
  ];

  // The latch on the Pi's DISPLAY connector lifts while the ribbon goes in.
  const piLatch = new THREE.Group();
  box(piLatch, M.white, [0, 0, 0], [2.6, 22, 0.8], 0.1);
  track(piLatch, [
    key(4.02, "pi", [2.7, 17, PI_TOP + 5.5]),
    key(4.2, "pi", [2.7, 17, PI_TOP + 6.9]),
    key(4.86, "pi", [2.7, 17, PI_TOP + 6.9]),
    key(4.98, "pi", [2.7, 17, PI_TOP + 5.5]),
  ]);

  // The display connector's latch does the same.
  const fpcLatch = new THREE.Group();
  box(fpcLatch, M.latch, [0, 0, 0], [1.3, 19, 0.9], 0.1);
  const latchAt = [wx(FPC.bx) + 1.2, wy(FPC.by) - 9.5];
  track(fpcLatch, [
    key(3.05, "dev", [...latchAt, 1.9]),
    key(3.25, "dev", [...latchAt, 3.3]),
    key(3.8, "dev", [...latchAt, 3.3]),
    key(3.95, "dev", [...latchAt, 1.9]),
  ]);

  // The microSD card: on the table, label up, then carried to the turned-over Pi and pushed in.
  const cardFlat = Q(1, 0, 0, 180);
  track(buildSdCard(), [
    key(0.38, "dev", [150, -58, TABLE + 0.85], cardFlat),
    key(0.6, "pi", [-22, 22.5, -1.05], IDENT, { lift: 16 }),
    key(0.68, "pi", [-22, 22.5, -1.05]),
    key(0.92, "pi", [-2.5, 22.5, -1.05]),
  ]);

  // Standoffs, one after another: carried over, then turned down into the display.
  PI.holes.forEach(([lx, ly], i) => {
    const t = 1.05 + i * 0.2;
    const at = [wx(PI_ORIGIN.bx - lx), wy(PI_ORIGIN.by - ly)];
    track(buildStandoff(), [
      key(t, "dev", [14 + i * 13, -74, TABLE + 4]),
      key(t + 0.13, "dev", [...at, 4.2], IDENT, { lift: 26 }),
      key(t + 0.3, "dev", [...at, 0], IDENT, { turns: 4, linear: true }),
    ]);
  });

  // Screws, the same way, into the standoffs through the Pi's corners.
  PI.holes.forEach(([lx, ly], i) => {
    const t = 2.5 + i * 0.11;
    track(buildScrew(), [
      key(t, "dev", [74 + i * 10, -74, TABLE + 5], Z180),
      key(t + 0.07, "pi", [lx, ly, PI_TOP + 5.2], IDENT, { lift: 24 }),
      key(t + 0.16, "pi", [lx, ly, PI_TOP], IDENT, { turns: 5, linear: true }),
    ]);
  });

  // The microphone, carried to the USB port and pushed in.
  const usbAt = [67.5, 31.5, PI_TOP + 3.5];
  track(buildMicrophone(), [
    key(5.05, "dev", [-82, -72, TABLE + 3.6]),
    key(5.58, "pi", [usbAt[0] + 15, usbAt[1], usbAt[2]], IDENT, { lift: 22 }),
    key(5.66, "pi", [usbAt[0] + 15, usbAt[1], usbAt[2]]),
    key(5.9, "pi", usbAt),
  ]);

  // The legs, one after the other: each is set on a bottom corner of the back, then its long
  // black screw is turned in through it.
  const blackScrew = std({ color: 0x1a1a1c, metalness: 0.6, roughness: 0.45 });
  [wx(5), wx(DISPLAY.w - 5)].forEach((x, i) => {
    const t = 6.05 + i * 0.46;
    track(buildLeg(), [
      key(t, "dev", [-112, -42 + i * 26, TABLE]),
      key(t + 0.17, "dev", [x, LEG_HOLE_Y, 0], IDENT, { lift: 30 }),
    ]);
    track(buildScrew(12, blackScrew), [
      key(t + 0.17, "dev", [-112, 16 + i * 12, TABLE + 12]),
      key(t + 0.29, "dev", [x, LEG_HOLE_Y, LEG.block + 11.5], IDENT, { lift: 22 }),
      key(t + 0.44, "dev", [x, LEG_HOLE_Y, LEG.block], IDENT, { turns: 6, linear: true }),
    ]);
  });

  // The power plug, from behind the display.
  const plug = track(buildPowerPlug(), [
    key(7.05, "dev", [20, 92, TABLE + 3.5], Z180),
    key(7.58, "pi", [10.6, -15, PI_TOP + 1.4], IDENT, { lift: 16 }),
    key(7.66, "pi", [10.6, -15, PI_TOP + 1.4]),
    key(7.9, "pi", [10.6, -1, PI_TOP + 1.4]),
  ]);

  const ribbon = buildRibbon();
  device.add(ribbon);

  // The cable trails from the plug to somewhere off the back of the table.
  const cable = new THREE.Mesh(new THREE.BufferGeometry(), M.cable);
  cable.castShadow = true;
  cable.frustumCulled = false;
  scene.add(cable);
  const cableEnd = new THREE.Vector3(60, 1.8, -520);
  // The cable is soft: it leaves the plug, sags straight away, lies on whatever is under it
  // (the back of the display while that is lying down, otherwise the table) and runs off
  // behind the display.
  const up = new THREE.Vector3(0, 1, 0);
  const _local = new THREE.Vector3();
  function restHeight(p) {
    const lyingFlat = new THREE.Vector3(0, 0, 1).applyQuaternion(device.quaternion).dot(up) > 0.7;
    _local.copy(p);
    device.worldToLocal(_local);
    const overDisplay = Math.abs(_local.x) < DISPLAY.w / 2 + 6 && Math.abs(_local.y) < DISPLAY.h / 2 + 6;
    return 1.8 + (lyingFlat && overDisplay ? -TABLE : 0);
  }
  function layCable() {
    device.updateMatrixWorld(true);
    const a = plug.localToWorld(new THREE.Vector3(...PLUG_CABLE));
    const out = plug.localToWorld(new THREE.Vector3(PLUG_CABLE[0] - 1, PLUG_CABLE[1], 0)).sub(a);
    const points = [a, a.clone().addScaledVector(out, 7)];
    // Where it starts to hang, a little out from the plug.
    const bend = a.clone().addScaledVector(out, 13);
    const fall = Math.max(0, bend.y - restHeight(bend));
    bend.y -= Math.min(fall * 0.5, 12);
    points.push(bend);
    const down = bend.clone();
    down.z -= 5;
    down.y = (bend.y + restHeight(down)) / 2;
    points.push(down);
    // Then along the floor, away behind the display.
    for (const back of [13, 30, 44, 70, 110]) {
      const p = bend.clone();
      p.z -= back;
      p.x += (cableEnd.x - p.x) * (back / 900);
      p.y = restHeight(p);
      points.push(p);
    }
    points.push(cableEnd);
    const path = new THREE.CatmullRomCurve3(points, false, "centripetal");
    cable.geometry.dispose();
    cable.geometry = new THREE.TubeGeometry(path, 80, 1.7, 10, false);
  }

  scene.traverse((o) => {
    if (o.isMesh && o !== table) {
      o.castShadow = true;
      o.receiveShadow = true;
    }
  });

  function applyTime(T) {
    place(device, deviceKeys, T, null);
    place(pi, piKeys, T, null);
    for (const { obj, keys } of tracks) place(obj, keys, T, pi);
    ribbon.userData.update(T);
    display.screenMat.color.setScalar(clamp01((T - 8.75) * 5));
    layCable();
  }

  // `time` moves steadily towards `step`.
  let step = finished ? STEPS.length - 1 : 0;
  let time = step;
  let from = 0;
  let started = 0;
  let length = 1;
  const camFrom = { pos: new THREE.Vector3(), target: new THREE.Vector3() };
  const camTo = { pos: new THREE.Vector3(), target: new THREE.Vector3() };
  let camMoving = false;

  function goTo(n) {
    n = Math.max(0, Math.min(STEPS.length - 1, n));
    from = time;
    step = n;
    started = performance.now();
    const gap = Math.abs(n - from);
    // A step with more to do (`pace`) takes longer; a jump over several steps plays faster.
    const pace = STEPS[Math.max(n, Math.ceil(from))].pace ?? 1;
    length = reduceMotion ? 1 : 3400 * (gap <= 1 ? Math.max(gap, 0.3) * pace : 1 + (gap - 1) * 0.3);
    camFrom.pos.copy(camera.position);
    camFrom.target.copy(controls.target);
    camTo.pos.set(...STEPS[n].cam[0]);
    camTo.target.set(...STEPS[n].cam[1]);
    camMoving = true;
    onStep?.(n);
  }

  const FINISHED_VIEW = { target: [0, 46, 0], distance: 285, elevation: 13, azimuth: 28 };
  const lookFrom = (azimuth) => view(FINISHED_VIEW.target, FINISHED_VIEW.distance, FINISHED_VIEW.elevation, azimuth);
  const first = finished ? lookFrom(FINISHED_VIEW.azimuth) : STEPS[0].cam;
  camera.position.set(...first[0]);
  controls.target.set(...first[1]);
  applyTime(time);
  onStep?.(step);

  function resize() {
    // The canvas can be wider than its container (the top picture runs past its column).
    const w = renderer.domElement.clientWidth || container.clientWidth;
    const h = renderer.domElement.clientHeight || container.clientHeight;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    // Keep the whole table of parts in view on narrow screens.
    camera.zoom = Math.min(1, camera.aspect / (finished ? 1.5 : 1.1));
    camera.updateProjectionMatrix();
  }
  new ResizeObserver(resize).observe(container);
  resize();

  renderer.setAnimationLoop((now) => {
    const t = clamp01((now - started) / length);
    if (time !== step) {
      time = t >= 1 ? step : from + (step - from) * t;
      applyTime(time);
    }
    if (camMoving) {
      // The camera gets there in the first part of the step, then the parts finish moving.
      const c = clamp01((now - started) / Math.min(length, 1500));
      camera.position.lerpVectors(camFrom.pos, camTo.pos, ease(c));
      controls.target.lerpVectors(camFrom.target, camTo.target, ease(c));
      if (c >= 1) camMoving = false;
    }
    // The finished picture sways a little, until someone turns it themselves.
    if (finished && !touched && !reduceMotion) {
      camera.position.set(...lookFrom(FINISHED_VIEW.azimuth + Math.sin(now / 3200) * 7)[0]);
    }
    controls.update();
    renderer.render(scene, camera);
  });

  return { goTo, get step() { return step; }, get length() { return length; }, count: STEPS.length };
}
