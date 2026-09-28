import * as THREE from 'three';
import { gsap } from 'gsap';
import { ScrollTrigger } from 'gsap/ScrollTrigger';

gsap.registerPlugin(ScrollTrigger);

// The specimen sheet holds three panels side by side, each an Arctic tern
// traced from a photograph: in flight, back with its catch, and a flock.
// Green holds the ink density; red is lowered where the bill and feet are,
// so the drawing can tint them.
const PANEL = { tern: 0, catch: 1 / 3, flock: 2 / 3 };

function paperRGB() {
  const value = getComputedStyle(document.documentElement).getPropertyValue('--paper').trim() || '#f3f5f4';
  const n = parseInt(value.replace('#', ''), 16);
  return [(n >> 16 & 255) / 255, (n >> 8 & 255) / 255, (n & 255) / 255];
}

// A drawing of the specimen in the tern's cap-black on the page's paper, its
// bill and feet in the bill's red, under a pane of glass that drifts across
// it: a bevelled edge that bends what is below, a slight colour split, and a
// lit rim. Now and then the drawing resolves into a red dot scan in steps,
// holds, and settles back.
const fragment = `
precision highp float;
uniform sampler2D specimen;
uniform vec2 resolution;
uniform vec2 pointer;
uniform float time;
uniform float phase;
uniform float crop;
uniform vec3 paper;
varying vec2 vUv;
mat2 rot(float a){return mat2(cos(a),-sin(a),sin(a),cos(a));}
float sdBox(vec2 p){vec2 q=abs(p)-vec2(.265,.31)+.035;return length(max(q,0.))+min(max(q.x,q.y),0.)-.035;}
vec3 sampleField(vec2 p){
  p=rot(sin(time*.16)*.065)*p;
  vec2 uv=p*vec2(1.3,.975)+.5;
  if(any(lessThan(uv,vec2(0.)))||any(greaterThan(uv,vec2(1.)))) return paper;
  vec2 cells=vec2(92.,120.);
  vec2 quantized=(floor(uv*cells)+.5)/cells;
  vec2 readUV=mix(uv,quantized,phase);
  vec3 texel=texture2D(specimen,vec2(readUV.x/3.+crop,readUV.y)).rgb;
  float value=texel.g;
  float red=clamp((texel.g-texel.r)/max(texel.g,.05)*1.6,0.,1.);
  float signal=smoothstep(.045,.88,value);
  vec2 sub=fract(uv*cells);
  float dots=step(length((sub-.5)*vec2(1.,.75)),.34);
  signal=mix(signal,step(.16,signal)*dots,phase);
  vec3 cap=vec3(.082,.098,.122);
  vec3 bill=vec3(.784,.149,.169);
  vec3 ink=mix(mix(cap,bill,red),bill,phase);
  if(red>.05) signal=max(signal,.85*red);
  return mix(paper,ink,signal);
}
void main(){
  vec2 p=vUv-.5;p.x*=resolution.x/resolution.y;
  vec2 center=vec2(sin(time*.28)*.11,cos(time*.19)*.035)+pointer*.045;
  float angle=.16+sin(time*.14)*.07;
  vec2 local=rot(angle)*(p-center);
  float d=sdBox(local);
  float aa=max(fwidth(d),.0004);
  float inside=1.-smoothstep(-aa,aa,d);
  float eps=.0003;
  vec2 n=normalize(vec2(sdBox(local+vec2(eps,0.))-sdBox(local-vec2(eps,0.)),sdBox(local+vec2(0.,eps))-sdBox(local-vec2(0.,eps)))+vec2(.000001));
  n=rot(-angle)*n;
  float depth=clamp(-d/.045,0.,1.);
  float bevel=sin(depth*3.14159265)*inside;
  vec2 bend=n*(bevel*.023+inside*.002);
  vec3 col;
  col.r=sampleField(p-bend*1.035).r;
  col.g=sampleField(p-bend).g;
  col.b=sampleField(p-bend*.965).b;
  float rim=1.-smoothstep(aa,aa*2.2,abs(d));
  float light=dot(n,normalize(vec2(-.6,.8)));
  col=mix(col,light>0.?vec3(1.):vec3(.40,.44,.50),rim*(light>0.?.72:.30));
  gl_FragColor=vec4(col,1.);
}`;

function makeSpecimen(renderer, host, texture) {
  const scene = new THREE.Scene();
  const camera = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1);
  const uniforms = {
    specimen: { value: texture },
    resolution: { value: new THREE.Vector2(1, 1) },
    pointer: { value: new THREE.Vector2() },
    time: { value: 3 },
    phase: { value: 0 },
    crop: { value: PANEL[host.dataset.scene] ?? 0 },
    paper: { value: new THREE.Vector3(...paperRGB()) },
  };
  const material = new THREE.ShaderMaterial({
    uniforms,
    vertexShader: 'varying vec2 vUv;void main(){vUv=uv;gl_Position=vec4(position.xy,0.,1.);}',
    fragmentShader: fragment,
  });
  scene.add(new THREE.Mesh(new THREE.PlaneGeometry(2, 2), material));
  const timeline = gsap.timeline({ paused: true, repeat: -1, repeatDelay: 2 });
  timeline.to(uniforms.phase, { value: 1, duration: 1.1, ease: 'steps(8)' }, 2.4)
    .to(uniforms.phase, { value: 0, duration: 1.3, ease: 'power2.inOut' }, 5.2);
  return {
    timeline,
    resize(w, h) { uniforms.resolution.value.set(w, h); },
    draw(time, pointer) { uniforms.time.value = time; uniforms.pointer.value.copy(pointer); renderer.render(scene, camera); },
  };
}

export async function startOptics(hosts, button) {
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  let paused = reduced.matches, time = 3, last = 0;
  const records = [];
  const entrances = [];
  // The pause control sits beside the animation it pauses.
  document.querySelector('.hero').append(button);
  button.classList.add('hero-motion-toggle');
  const texture = await new THREE.TextureLoader().loadAsync('./specimens.jpg');
  const setLabel = () => { button.textContent = paused ? 'Resume motion' : 'Pause motion'; button.setAttribute('aria-pressed', String(paused)); };
  const sync = () => {
    for (const a of entrances) { if (paused || document.hidden) a.pause(); else if (a.progress() < 1) a.resume(); }
    for (const r of records) { if (paused || document.hidden || !r.visible) r.engine.timeline.pause(); else r.engine.timeline.play(); }
  };
  const visibility = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      let r = records.find((x) => x.host === entry.target);
      if (!r && entry.isIntersecting) {
        try {
          const renderer = new THREE.WebGLRenderer({ canvas: entry.target.querySelector('canvas'), antialias: true, powerPreference: 'low-power' });
          renderer.setPixelRatio(Math.min(Math.max(devicePixelRatio, 1.5), 2));
          const engine = makeSpecimen(renderer, entry.target, texture);
          r = { host: entry.target, renderer, engine, visible: true, pointer: new THREE.Vector2(), target: new THREE.Vector2() };
          records.push(r);
          new ResizeObserver(() => {
            const { width, height } = r.host.getBoundingClientRect();
            renderer.setSize(width, height, false); engine.resize(width, height); engine.draw(time, r.pointer);
          }).observe(r.host);
          r.host.addEventListener('pointermove', (e) => {
            const rect = r.host.getBoundingClientRect();
            r.target.set((e.clientX - rect.left) / rect.width - .5, .5 - (e.clientY - rect.top) / rect.height);
          });
          r.host.addEventListener('pointerleave', () => r.target.set(0, 0));
          r.host.querySelector('canvas').addEventListener('webglcontextlost', (e) => { e.preventDefault(); r.visible = false; r.engine.timeline.pause(); r.host.classList.remove('ready'); });
          r.host.classList.add('ready');
        } catch { entry.target.classList.add('static-only'); }
      }
      if (r) r.visible = entry.isIntersecting;
    }
    sync();
  }, { rootMargin: '100px' });
  hosts.forEach((host) => visibility.observe(host));
  gsap.ticker.add(() => {
    const now = performance.now();
    if (now - last < 32) return;
    const delta = last ? Math.min((now - last) / 1000, .06) : 0;
    last = now;
    if (!paused && !document.hidden) time += delta;
    for (const r of records) {
      if (!r.visible || document.hidden || paused) continue;
      r.pointer.lerp(r.target, .08);
      r.engine.draw(time, r.pointer);
    }
  });
  button.addEventListener('click', () => { paused = !paused; setLabel(); sync(); });
  reduced.addEventListener('change', () => { paused = reduced.matches; setLabel(); sync(); });
  document.addEventListener('visibilitychange', sync);
  gsap.matchMedia().add('(prefers-reduced-motion: no-preference)', () => {
    const entrance = gsap.timeline({ defaults: { ease: 'power2.out' } });
    entrance.from('.hero h1', { y: 24, duration: .65 })
      .from('.hero > .optics', { y: 22, scale: .965, duration: .85 }, .24)
      .from('.hero-bottom', { y: 12, duration: .5 }, .7);
    entrances.push(entrance);
    document.querySelectorAll('.feature-list article, .features article, .steps article').forEach((el) =>
      gsap.from(el, { y: 18, duration: .6, ease: 'power2.out', scrollTrigger: { trigger: el, start: 'top 94%', once: true } }));
  });
  setLabel();
}
