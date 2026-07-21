/*
 * cat-brain — the Pocket Cat harness POLICY, running as the QuickJS guest.
 *
 * The native Rust host (main.rs) owns per-frame work: the window, the
 * framebuffer, the clock, input, and scene rotation (Law 1 — per-frame work
 * stays in the core). This guest owns POLICY: how the cat reacts to what the
 * host observes, whether to avert on private content, and how it answers
 * commands. Host → guest via __cat_event(json); guest → host via the mounted
 * `cat` surface ops. No DOM, no deps — pure QuickJS.
 */

const S = { observe: true, privacy: true, napping: false, browsing: false };

const AVERT = ["LOOKING AWAY", "PRIVACY - HIDDEN", "NOT PEEKING", "COVERING EYES"];
const PET = ["MEOW", "PURR", "NYA~", "PET ME MORE"];

function watch() {
  if (S.napping) return;
  cat.avert(false);
  cat.fx("none");
  cat.state("idle");
  cat.cad(2);
}

globalThis.__cat_event = function (json) {
  const ev = JSON.parse(json);
  switch (ev.t) {
    case "boot":
      cat.observe(true);
      cat.privacy(true);
      cat.state("talk");
      cat.say("HI - IM WATCHING", 3000);
      cat.cad(14);
      setTimeout(watch, 1600);
      break;

    case "tick":
      // host hands us the scene it is mirroring + whether it is safe
      if (!S.observe || S.napping || S.browsing) break;
      if (!ev.safe && S.privacy) {
        cat.avert(true);
        cat.state("idle");
        cat.fx("none");
        cat.say(AVERT[(Math.random() * AVERT.length) | 0], 2200);
        cat.cad(2);
      } else if (!ev.safe) {
        cat.avert(false);
        cat.state("work");
        cat.say("YOU LET ME LOOK", 2000);
      } else {
        watch();
      }
      break;

    case "pet":
      if (S.napping) { S.napping = false; cat.observe(true); watch(); cat.say("IM UP", 1500); break; }
      if (!S.browsing) {
        cat.state("excited");
        cat.fx("heart");
        cat.cad(14);
        cat.say(PET[(Math.random() * PET.length) | 0], 1600);
        setTimeout(watch, 1000);
      }
      break;

    case "browse_done":
      S.browsing = false;
      cat.state("excited");
      cat.fx("heart");
      cat.say("STARRED IT", 2000);
      setTimeout(watch, 1500);
      break;

    case "menu":
      handleMenu(ev.act);
      break;

    case "cmd":
      handleCmd(String(ev.text || "").toLowerCase());
      break;
  }
};

function handleMenu(act) {
  if (act === "observe") { S.observe = !S.observe; cat.observe(S.observe);
    if (S.observe) { watch(); cat.say("WATCHING AGAIN", 1600); }
    else { cat.avert(false); cat.fx("none"); cat.state("idle"); cat.cad(0); cat.say("OK NOT LOOKING", 1600); } }
  else if (act === "privacy") { S.privacy = !S.privacy; cat.privacy(S.privacy);
    if (!S.privacy) cat.avert(false);
    cat.say(S.privacy ? "PRIVACY ON" : "PRIVACY OFF", 1800); }
  else if (act === "browse") { startBrowse(); }
  else if (act === "nap") { nap(); }
}

function handleCmd(t) {
  if (S.napping) { S.napping = false; cat.observe(true); }
  cat.state("talk");
  let reply = "OK - NOTED";
  if (/stop|dont|off/.test(t)) { S.observe = false; cat.observe(false); cat.cad(0); reply = "STOPPED"; }
  else if (/watch|look|see/.test(t)) { S.observe = true; cat.observe(true); reply = "WATCHING"; }
  else if (/nap|sleep/.test(t)) { setTimeout(nap, 400); reply = "NAPPING"; }
  else if (/star|browse|go/.test(t)) { setTimeout(startBrowse, 300); reply = "ON IT"; }
  else if (/privacy/.test(t)) { S.privacy = !S.privacy; cat.privacy(S.privacy); reply = S.privacy ? "PRIVACY ON" : "PRIVACY OFF"; }
  else if (/hi|hello/.test(t)) { reply = "MEOW - IM HERE"; }
  cat.say(reply, 2600);
  cat.fx("heart");
  cat.cad(14);
  setTimeout(function () { if (!S.napping && !S.browsing) watch(); }, 1400);
}

function startBrowse() {
  if (S.browsing) return;
  S.browsing = true;
  cat.avert(false);
  cat.state("work");
  cat.say("DRIVING - WATCH", 1800);
  cat.cad(14);
  cat.browse(); // host runs the browser-use scene, then sends browse_done
}

function nap() {
  S.napping = true;
  S.observe = false;
  cat.observe(false);
  cat.avert(false);
  cat.state("sleep");
  cat.fx("zzz");
  cat.cad(0);
  cat.say("ZZZ", 1500);
}
