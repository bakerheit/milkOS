
const C={milk:'#FFFDF8',cream:'#FBF6EC',oat:'#EFE6D4',sand:'#E2D5BD',latte:'#C4AE8F',cocoa:'#7A6652',esp:'#3A2E28',straw:'#E58C97',blue:'#7F93C9',matcha:'#9CBF9B',honey:'#EBB650',lav:'#B9A9D6',peach:'#F2B79B'};
const B=(x,y,w,h,s='',inner='')=>`<div style="position:absolute;left:${x}px;top:${y}px;width:${w}px;height:${h}px;${s}">${inner}</div>`;
const T=(x,y,t,s='')=>`<div style="position:absolute;left:${x}px;top:${y}px;white-space:pre;${s}">${t}</div>`;
const TC=(x,y,w,t,s='')=>`<div style="position:absolute;left:${x}px;top:${y}px;width:${w}px;text-align:center;white-space:pre;${s}">${t}</div>`;
const TR=(xr,y,t,s='')=>`<div style="position:absolute;right:${1024-xr}px;top:${y}px;white-space:pre;text-align:right;${s}">${t}</div>`;
const TRi=(w,xr,y,t,st='')=>`<div style="position:absolute;right:${w-xr}px;top:${y}px;white-space:pre;text-align:right;${st}">${t}</div>`;
const X2='font-size:26.667px;line-height:32px;';
const X3='font-size:40px;line-height:48px;';
const X4='font-size:53.333px;line-height:64px;';
const BD='font-weight:700;';

/* ---------- icons: tile + a few primitives each ---------- */
function icon(x,y,s,kind,bg=C.milk,fg=C.esp,extra=''){
  const u=s/48; const r=Math.round(s*.24);
  let g='';
  const R=(a,b,w,h,st='')=>B(a*u,b*u,w*u,h*u,st);
  if(kind==='folder'){g=R(9,14,14,6,`background:${fg};border-radius:${3*u}px ${3*u}px 0 0`)+R(9,19,30,18,`background:${fg};border-radius:${3*u}px`);}
  if(kind==='term'){g=T(8*u,13*u,'&gt;_',`font-weight:700;color:${fg};font-size:${Math.max(10,18*u)}px;line-height:${20*u}px`);}
  if(kind==='gear'){g=R(11,11,26,26,`border:${5*u}px solid ${fg};border-radius:50%`)+R(20,20,8,8,`background:${fg};border-radius:50%`);}
  if(kind==='note'){g=R(12,11,24,26,`border:${2*u}px solid ${fg};border-radius:${3*u}px`)+R(17,18,14,2*u,`background:${fg}`)+R(17,24,14,2*u,`background:${fg}`)+R(17,30,9,2*u,`background:${fg}`);}
  if(kind==='globe'){g=R(10,10,28,28,`border:${3*u}px solid ${fg};border-radius:50%`)+R(21,10,6,28,`border:${2*u}px solid ${fg};border-radius:50%`)+R(10,23,28,2*u,`background:${fg}`);}
  if(kind==='music'){g=R(14,28,11,9,`background:${fg};border-radius:50%`)+R(23,11,3,23,`background:${fg}`)+R(23,11,13,5,`background:${fg};border-radius:2px`);}
  if(kind==='clock'){g=R(10,10,28,28,`border:${3*u}px solid ${fg};border-radius:50%`)+R(23,16,2.5,9,`background:${fg}`)+R(23,23,8,2.5,`background:${fg}`);}
  if(kind==='photo'){g=R(9,12,30,24,`border:${3*u}px solid ${fg};border-radius:${4*u}px`)+R(16,17,8,8,`background:${C.honey};border-radius:50%`)+R(14,29,22,5,`background:${C.matcha}`);}
  if(kind==='calc'){g=R(11,11,11,11,`background:${fg};border-radius:3px`)+R(26,11,11,11,`background:${fg};border-radius:3px`)+R(11,26,11,11,`background:${fg};border-radius:3px`)+R(26,26,11,11,`background:${C.straw};border-radius:3px`);}
  if(kind==='cal'){g=R(10,12,28,26,`border:${3*u}px solid ${fg};border-radius:${4*u}px`)+R(10,18,28,3*u,`background:${fg}`)+R(17,26,5,5,`background:${C.straw}`);}
  if(kind==='doc'){g=R(14,9,20,30,`border:${3*u}px solid ${fg};border-radius:${3*u}px`)+R(19,17,10,2*u,`background:${fg}`)+R(19,23,10,2*u,`background:${fg}`);}
  return B(x,y,s,s,`background:${bg};border-radius:${r}px;overflow:hidden;${extra}`,g);
}
function lights(x,y,d=12){return B(x,y,d,d,`background:${C.straw};border-radius:50%`)+B(x+d+6,y,d,d,`background:${C.honey};border-radius:50%`)+B(x+2*(d+6),y,d,d,`background:${C.matcha};border-radius:50%`);}
function drop(x,y,s,fg=C.esp){return B(x,y,s,s,`border-radius:50%;background:${C.milk};border:${Math.max(2,s/6)}px solid ${fg}`);}

/* ---------- 01 Whole Milk (macOS-like) ---------- */
function s01(){
  let h=B(0,0,1024,768,`background:linear-gradient(160deg,#F9EBD3 0%,#EBD7BB 55%,#D9C3A5 100%)`);
  h+=B(560,-140,560,560,'border-radius:50%;background:rgba(255,253,248,.38)');
  h+=B(-160,420,520,520,`border-radius:50%;background:rgba(229,140,151,.16)`);
  // menu bar
  h+=B(0,0,1024,24,`background:rgba(255,253,248,.82);border-bottom:1px solid ${C.sand}`);
  h+=drop(12,6,13)+T(38,4,'<b>Files</b>   File   Edit   View   Go   Window   Help');
  h+=TR(1012,4,'Wi-Fi   87%   Tue 10:41');
  // Files window
  h+=B(96,62,600,404,`background:${C.milk};border-radius:12px;border:1px solid ${C.sand};box-shadow:0 18px 40px rgba(90,64,40,.22);overflow:hidden`,
    B(0,0,600,34,`background:${C.cream};border-bottom:1px solid ${C.sand}`)+lights(14,11)+TC(150,9,300,'<b>Documents</b>')+
    B(0,34,148,370,`background:${C.oat};border-right:1px solid ${C.sand}`)+
    T(14,48,'FAVOURITES',`color:${C.cocoa};font-size:11px`)+
    B(8,70,132,24,`background:${C.straw}55;border-radius:6px`)+T(18,74,'Documents')+T(18,102,'Downloads')+T(18,130,'Desktop')+T(18,158,'Pictures')+T(18,186,'Music')+
    T(14,222,'DEVICES',`color:${C.cocoa};font-size:11px`)+T(18,246,'milk-ssd')+T(18,274,'Network')+
    icon(180,52,56,'folder',C.honey+'55',C.cocoa)+TC(168,112,80,'projects')+
    icon(280,52,56,'folder',C.honey+'55',C.cocoa)+TC(268,112,80,'school')+
    icon(380,52,56,'doc',C.milk,C.esp,`border:1px solid ${C.sand}`)+TC(368,112,80,'todo.txt')+
    icon(480,52,56,'photo',C.milk,C.esp,`border:1px solid ${C.sand}`)+TC(468,112,80,'bike.png')+
    icon(180,160,56,'note',C.milk,C.esp,`border:1px solid ${C.sand}`)+TC(168,220,80,'ideas.md')+
    icon(280,160,56,'folder',C.honey+'55',C.cocoa)+TC(268,220,80,'kernel')+
    icon(380,160,56,'doc',C.milk,C.esp,`border:1px solid ${C.sand}`)+TC(368,220,80,'notes.txt')+
    B(148,380,452,24,`background:${C.cream};border-top:1px solid ${C.sand}`)+T(160,384,'7 items, 212 GB available',`color:${C.cocoa}`)
  );
  // Terminal
  h+=B(440,270,520,290,`background:${C.esp};border-radius:12px;box-shadow:0 18px 40px rgba(60,40,30,.35);overflow:hidden`,
    B(0,0,520,30,`background:#4A3C34`)+lights(14,9)+TC(110,7,300,'milk &mdash; sh',`color:${C.sand}`)+
    T(14,46,'milk@milk ~ % uname -a',`color:${C.cream}`)+T(14,64,'milkOS 0.1 aarch64 milk',`color:${C.latte}`)+
    T(14,90,'milk@milk ~ % ls',`color:${C.cream}`)+T(14,108,'projects  school  todo.txt  ideas.md',`color:${C.matcha}`)+
    T(14,134,'milk@milk ~ % ',`color:${C.cream}`)+B(142,134,8,16,`background:${C.cream}`)
  );
  // dock
  const items=[['folder',C.honey],['term',C.esp],['globe',C.blue],['note',C.milk],['music',C.straw],['photo',C.milk],['cal',C.milk],['gear',C.latte]];
  const dw=items.length*60+24, dx=(1024-dw)/2;
  h+=B(dx,684,dw,68,`background:rgba(255,253,248,.62);border:1px solid rgba(255,255,255,.8);border-radius:20px;box-shadow:0 8px 24px rgba(90,64,40,.18)`);
  items.forEach((it,i)=>{h+=icon(dx+14+i*60,692,48,it[0],it[1],it[0]==='term'?C.cream:C.esp);});
  h+=B(dx+14+20,744,8,4,`background:${C.cocoa};border-radius:2px`)+B(dx+14+60+20,744,8,4,`background:${C.cocoa};border-radius:2px`);
  return h;
}

/* ---------- 02 Pail (tiling) ---------- */
function s02(){
  let h=B(0,0,1024,768,`background:${C.oat}`);
  const tile=(x,y,w,hh,title,body,focus)=>B(x,y,w,hh,`background:${C.milk};border:${focus?3:1}px solid ${focus?C.honey:C.latte};border-radius:4px;overflow:hidden`,
     B(0,0,w,20,`background:${focus?C.honey+'55':C.cream};border-bottom:1px solid ${C.sand}`)+T(8,2,title,focus?BD:`color:${C.cocoa}`)+body);
  h+=tile(8,8,500,720,'1:edit  kernel/src/main.rs',
     [..."use core::fmt::Write;|mod mem;|mod display;|mod irq;||#[no_mangle]|pub extern \"C\" fn kmain() -> ! {|    uart::init();|    mem::init(dtb);|    display::init();|    println!(\"milkOS up\");|    loop { wfi(); }|}"].join('').split('|').map((l,i)=>T(8,30+i*18,`<span style="color:${C.latte}">${String(i+1).padStart(2,' ')}</span> ${l.replace(/</g,'&lt;').replace(/>/g,'&gt;')}`)).join('')+
     B(0,700-0,500,20,`background:${C.cream};border-top:1px solid ${C.sand}`)+T(8,702,'-- NORMAL --   main.rs   12:5',`color:${C.cocoa}`),true);
  h+=tile(516,8,500,360,'2:term  ~/milkOS',
     T(8,30,'$ scripts/run.sh',`color:${C.cocoa}`)+T(8,48,'   Compiling milkos-kernel v0.1.0')+T(8,66,'   Finished dev [unoptimized] in 2.1s')+T(8,84,'milkOS up  (qemu virt, 512M)')+T(8,102,'fb 1024x768x32 @ 0x4000_0000')+T(8,120,'$ _'),false);
  h+=tile(516,376,246,352,'3:top',
     T(8,30,'cpu  ')+B(48,32,180,12,`background:${C.sand}`,B(0,0,56,12,`background:${C.matcha}`))+
     T(8,54,'mem  ')+B(48,56,180,12,`background:${C.sand}`,B(0,0,118,12,`background:${C.honey}`))+
     T(8,84,'PID  NAME',`color:${C.cocoa}`)+T(8,102,'  1  init')+T(8,120,'  2  shell')+T(8,138,'  5  edit')+T(8,156,'  9  disp'),false);
  h+=tile(770,376,246,352,'4:files',
     T(8,30,'projects/')+T(8,48,'school/')+T(8,66,'todo.txt')+T(8,84,'ideas.md')+B(0,100,246,18,`background:${C.straw}55`)+T(8,102,'kernel/'),false);
  // bar
  h+=B(0,736,1024,32,`background:${C.esp}`);
  [['1',1],['2',0],['3',0],['4',0],['5',0]].forEach((w,i)=>{h+=B(8+i*28,740,24,24,`background:${w[1]?C.honey:'transparent'};border-radius:4px`)+TC(8+i*28,744,24,w[0],`color:${w[1]?C.esp:C.latte};${w[1]?BD:''}`);});
  h+=TC(256,744,512,'edit  kernel/src/main.rs',`color:${C.cream}`);
  h+=TR(1012,744,'cpu 12%   mem 41%   Tue 10:41',`color:${C.sand}`);
  return h;
}

/* ---------- 03 Skim (launcher-first) ---------- */
function s03(){
  let h=B(0,0,1024,768,`background:linear-gradient(180deg,#FBF1DE,#EEDFC4)`);
  h+=T(48,40,'10:41',X4+BD+`color:${C.esp}`)+T(52,108,'Tuesday, 20 October',`color:${C.cocoa}`);
  h+=TR(976,48,'Wi-Fi   87%',`color:${C.cocoa}`);
  // dimmed background window
  h+=B(150,300,300,230,`background:${C.milk};border-radius:12px;border:1px solid ${C.sand};opacity:.5`,B(0,0,300,30,`background:${C.cream}`)+T(12,7,'notes.md')+T(12,44,'# ideas')+T(12,62,'- milk logo drop'));
  h+=B(0,0,1024,768,'background:rgba(251,241,222,.35)');
  // launcher
  h+=B(172,150,680,66,`background:${C.milk};border-radius:18px;border:1px solid ${C.sand};box-shadow:0 20px 50px rgba(90,64,40,.25)`,
     icon(16,13,40,'globe',C.cream,C.cocoa)+T(72,12,'mus<span style="background:#E5D9C0">&#8203;</span>',X2+`color:${C.esp}`)+B(122,14,4,32,`background:${C.esp}`)+TRi(680,662,22,'esc',`color:${C.latte}`));
  h+=B(172,228,680,330,`background:${C.milk};border-radius:18px;border:1px solid ${C.sand};box-shadow:0 20px 50px rgba(90,64,40,.2);overflow:hidden`,
     T(20,12,'APPLICATIONS',`color:${C.cocoa};font-size:11px`)+
     B(8,34,664,50,`background:${C.straw}44;border-radius:12px`)+icon(18,40,38,'music',C.straw,C.esp)+T(70,44,'<b>Music</b>')+T(70,62,'Play and organise songs',`color:${C.cocoa}`)+TRi(680,656,52,'Enter',`color:${C.cocoa}`)+
     icon(18,92,38,'doc',C.cream,C.esp,`border:1px solid ${C.sand}`)+T(70,96,'Musl notes.txt')+T(70,114,'~/school',`color:${C.cocoa}`)+
     T(20,160,'COMMANDS',`color:${C.cocoa};font-size:11px`)+
     icon(18,182,38,'gear',C.cream,C.esp,`border:1px solid ${C.sand}`)+T(70,186,'Mute sound')+T(70,204,'Settings &rsaquo; Audio',`color:${C.cocoa}`)+
     T(20,262,'RECENT',`color:${C.cocoa};font-size:11px`)+
     T(70,284,'bike.png        ideas.md        todo.txt',`color:${C.cocoa}`)
  );
  h+=TC(172,580,680,'Tab for actions   Cmd-Space to open anywhere   &gt; for commands   ? for help',`color:${C.cocoa}`);
  return h;
}

/* ---------- 04 Pour (touch) ---------- */
function s04(){
  let h=B(0,0,1024,768,`background:linear-gradient(170deg,#FCEFD9,#F2D9C4 60%,#E8C8D0)`);
  h+=T(24,6,'10:41',BD)+TR(1000,6,'Wi-Fi   87%');
  h+=B(262,40,500,56,`background:rgba(255,253,248,.8);border-radius:28px;border:1px solid ${C.sand}`)+T(290,56,'Search apps and files',`color:${C.latte}`);
  // widget
  h+=B(60,124,420,172,`background:rgba(255,253,248,.85);border-radius:28px;border:1px solid ${C.sand}`,T(28,22,'10:41',X4+BD)+T(28,92,'Tuesday, 20 October',`color:${C.cocoa}`)+T(28,120,'Next: Dentist, 2:30 PM',`color:${C.straw};${BD}`));
  h+=B(500,124,464,172,`background:rgba(255,253,248,.85);border-radius:28px;border:1px solid ${C.sand}`,
    T(28,22,'NOW PLAYING',`color:${C.cocoa};font-size:11px`)+T(28,48,'Cream Soda Skies',X2)+T(28,86,'The Lactose Intolerants',`color:${C.cocoa}`)+
    B(28,124,300,8,`background:${C.sand};border-radius:4px`,B(0,0,120,8,`background:${C.straw};border-radius:4px`))+
    B(360,100,64,64,`background:${C.straw};border-radius:32px`,B(26,20,8,24,`background:${C.milk}`)+B(40,20,8,24,`background:${C.milk}`)));
  const apps=[['folder','Files',C.honey],['term','Shell',C.esp],['globe','Web',C.blue],['note','Notes',C.milk],['music','Music',C.straw],['photo','Photos',C.milk],['cal','Calendar',C.milk],['gear','Settings',C.latte]];
  apps.forEach((a,i)=>{const col=i%4,row=Math.floor(i/4);const x=100+col*220,y=336+row*160;
    h+=icon(x,y,112,a[0],a[2],a[0]==='term'?C.cream:C.esp,'box-shadow:0 8px 18px rgba(90,64,40,.18)')+TC(x-20,y+124,152,a[1]);});
  h+=B(486,676,6,6,`background:${C.esp};border-radius:50%`)+B(502,676,6,6,`background:${C.latte};border-radius:50%`)+B(518,676,6,6,`background:${C.latte};border-radius:50%`);
  h+=B(412,742,200,6,`background:${C.esp};border-radius:3px`);
  return h;
}

/* ---------- 05 Curd (terminal-centric) ---------- */
function s05(){
  let h=B(0,0,1024,768,`background:${C.cream}`);
  h+=B(0,0,1024,24,`background:${C.esp}`);
  [['1:shell',1],['2:edit',0],['3:logs',0],['4:web',0]].forEach((t,i)=>{h+=B(i*104,0,104,24,`background:${t[1]?C.honey:'transparent'}`)+TC(i*104,4,104,t[0],`color:${t[1]?C.esp:C.sand};${t[1]?BD:''}`);});
  h+=TR(1012,4,'Tue 10:41   87%',`color:${C.sand}`);
  const pane=(x,y,w,hh,title,body,f)=>B(x,y,w,hh,`border:${f?2:1}px solid ${f?C.honey:C.latte};background:${C.milk}`,T(8,-9,`&nbsp;${title}&nbsp;`,`background:${C.cream};color:${f?C.esp:C.cocoa}`)+body);
  h+=pane(8,40,640,690,'shell',
    T(12,14,`<span style="color:${C.straw}">milk@milk</span> <span style="color:${C.blue}">~/milkOS</span> $ milk status`)+
    T(12,38,'kernel   milkOS 0.1.0  (aarch64)')+T(12,56,'uptime   2h 14m')+T(12,74,'disk     212 GB free')+
    T(12,104,`<span style="color:${C.straw}">milk@milk</span> <span style="color:${C.blue}">~/milkOS</span> $ ls`)+
    T(12,128,`<span style="color:${C.blue}">docs/</span>  <span style="color:${C.blue}">kernel/</span>  <span style="color:${C.blue}">scripts/</span>  Cargo.toml  README.md`)+
    T(12,158,`<span style="color:${C.straw}">milk@milk</span> <span style="color:${C.blue}">~/milkOS</span> $ `)+B(212,158,8,16,`background:${C.esp}`),true);
  h+=pane(660,40,356,270,'clock + disks',
    T(12,16,'10:41:07',X3+BD)+T(12,72,'Tuesday 20 October',`color:${C.cocoa}`)+
    T(12,108,'/       ')+B(80,110,240,12,`background:${C.sand}`,B(0,0,70,12,`background:${C.matcha}`))+
    T(12,132,'/Users   ')+B(80,134,240,12,`background:${C.sand}`,B(0,0,160,12,`background:${C.honey}`))+
    T(12,156,'/tmp    ')+B(80,158,240,12,`background:${C.sand}`,B(0,0,18,12,`background:${C.matcha}`)),false);
  h+=pane(660,330,356,400,'jobs',
    T(12,16,'ID  CMD          STATE',`color:${C.cocoa}`)+T(12,38,' 1  display      running')+T(12,56,' 2  input        running')+T(12,74,' 3  shell        running')+T(12,92,' 4  cargo build  done',`color:${C.matcha}`)+
    B(12,330,332,50,`background:${C.cream};border-radius:6px;border:1px solid ${C.sand}`,T(10,8,'Cargo build finished',BD)+T(10,26,'0 warnings',`color:${C.cocoa}`)),false);
  // palette
  h+=B(232,200,560,160,`background:${C.milk};border:2px solid ${C.esp};border-radius:8px;box-shadow:0 10px 0 rgba(58,46,40,.25)`,
    T(14,12,'&gt; open sett',`color:${C.esp}`)+B(100,12,8,16,`background:${C.esp}`)+
    B(6,40,546,26,`background:${C.honey}55`)+T(14,46,'Settings &mdash; Display')+T(14,76,'Settings &mdash; Network')+T(14,102,'Settings &mdash; Keyboard')+T(14,134,'Ctrl-P palette   Tab complete   Esc close',`color:${C.cocoa}`));
  return h;
}

/* ---------- 06 Today (widget home) ---------- */
function s06(){
  let h=B(0,0,1024,768,`background:${C.cream}`);
  h+=B(0,0,1024,28,`background:${C.cream}`)+drop(16,8,13)+T(40,6,'<b>milk</b>')+TR(1008,6,'Wi-Fi   87%   10:41');
  h+=T(48,56,'Good morning',X3+BD)+T(48,112,'Tuesday, 20 October   2 events today',`color:${C.cocoa}`);
  const card=(x,y,w,hh,c,t,body)=>B(x,y,w,hh,`background:${c};border-radius:22px;border:1px solid ${C.sand};overflow:hidden`,T(20,16,t,`color:${C.cocoa};font-size:11px;letter-spacing:1px`)+body);
  h+=card(48,156,300,220,C.milk,'CALENDAR',
    ['M','T','W','T','F','S','S'].map((d,i)=>TC(14+i*38,42,38,d,`color:${C.latte}`)).join('')+
    Array.from({length:21},(_,i)=>{const n=i+12,col=i%7,row=Math.floor(i/7);const today=n===20;return (today?B(14+col*38+5,66+row*34-3,28,24,`background:${C.straw};border-radius:12px`):'')+TC(14+col*38,66+row*34,38,String(n>31?n-31:n),today?`color:${C.milk};${BD}`:'');}).join(''));
  h+=card(368,156,300,220,C.honey+'66','NEXT UP',
    T(20,48,'2:30 PM',X2+BD)+T(20,88,'Dentist',X2)+T(20,128,'12 Elm St',`color:${C.cocoa}`)+T(20,170,'Then: Pick up bike (5:00)',`color:${C.cocoa}`));
  h+=card(688,156,288,220,C.milk,'SYSTEM',
    T(20,48,'cpu')+B(70,50,190,12,`background:${C.sand};border-radius:6px`,B(0,0,50,12,`background:${C.matcha};border-radius:6px`))+
    T(20,80,'mem')+B(70,82,190,12,`background:${C.sand};border-radius:6px`,B(0,0,110,12,`background:${C.honey};border-radius:6px`))+
    T(20,112,'disk')+B(70,114,190,12,`background:${C.sand};border-radius:6px`,B(0,0,40,12,`background:${C.blue};border-radius:6px`))+
    T(20,164,'Up 2h 14m',`color:${C.cocoa}`)+T(20,184,'All systems fine',`color:${C.matcha};${BD}`));
  h+=card(48,396,420,200,C.milk,'NOTES',
    T(20,44,'# milkOS',BD)+T(20,68,'- pick the UI direction')+T(20,86,'- font: try Spleen?')+T(20,104,'- boot logo, a milk drop')+T(20,140,'Edited yesterday',`color:${C.latte}`));
  h+=card(488,396,488,200,C.milk,'RECENT FILES',
    icon(20,44,56,'folder',C.honey+'55',C.cocoa)+TC(8,106,80,'projects')+icon(112,44,56,'doc',C.cream,C.esp,`border:1px solid ${C.sand}`)+TC(100,106,80,'todo.txt')+
    icon(204,44,56,'photo',C.cream,C.esp,`border:1px solid ${C.sand}`)+TC(192,106,80,'bike.png')+icon(296,44,56,'note',C.cream,C.esp,`border:1px solid ${C.sand}`)+TC(284,106,80,'ideas.md')+icon(388,44,56,'folder',C.honey+'55',C.cocoa)+TC(376,106,80,'kernel'));
  // dock-lite
  const d=['folder','term','globe','music','gear'];
  h+=B(362,680,300,64,`background:${C.milk};border-radius:32px;border:1px solid ${C.sand}`);
  d.forEach((k,i)=>{h+=icon(378+i*56,692,40,k,k==='term'?C.esp:C.cream,k==='term'?C.cream:C.esp,`border:1px solid ${C.sand}`);});
  return h;
}

/* ---------- 07 Spaces (overview) ---------- */
function s07(){
  let h=B(0,0,1024,768,`background:linear-gradient(160deg,#F9EBD3,#E3CFB2)`);
  h+=B(0,0,1024,768,'background:rgba(58,46,40,.35)');
  h+=B(0,0,1024,24,`background:rgba(255,253,248,.82)`)+drop(12,6,13)+T(38,4,'<b>Overview</b>')+TR(1012,4,'Tue 10:41');
  // spaces strip
  const sp=[['1 Code',1],['2 Web',0],['3 Notes',0],['+',0]];
  sp.forEach((s,i)=>{const x=232+i*140;h+=B(x,40,128,88,`background:${C.milk}${s[1]?'':'99'};border-radius:10px;border:${s[1]?3:1}px solid ${s[1]?C.honey:C.sand}`,
     B(8,8,50,34,`background:${C.cream};border-radius:4px;border:1px solid ${C.sand}`)+B(62,18,54,50,`background:${C.cream};border-radius:4px;border:1px solid ${C.sand}`)+TC(0,66,128,s[0],`font-size:11px;color:${C.cocoa}`));});
  const w=(x,y,ww,hh,title,body,sel)=>B(x,y,ww,hh,`background:${C.milk};border-radius:12px;border:${sel?4:1}px solid ${sel?C.honey:C.sand};box-shadow:0 14px 30px rgba(0,0,0,.3);overflow:hidden`,
     B(0,0,ww,26,`background:${C.cream};border-bottom:1px solid ${C.sand}`)+lights(10,7,10)+TC(60,5,ww-120,`<b>${title}</b>`,'font-size:12px')+body)+TC(x,y+hh+8,ww,title,`color:${C.cream}`);
  h+=w(60,170,300,210,'Files',icon(20,44,40,'folder',C.honey+'55',C.cocoa)+icon(80,44,40,'folder',C.honey+'55',C.cocoa)+icon(140,44,40,'doc',C.cream,C.esp)+icon(20,110,40,'photo',C.cream,C.esp),false);
  h+=w(390,170,300,210,'Shell',B(0,26,300,184,`background:${C.esp}`)+T(10,40,'$ cargo build',`color:${C.cream}`)+T(10,58,'Finished in 2.1s',`color:${C.matcha}`)+T(10,76,'$ _',`color:${C.cream}`),true);
  h+=w(720,170,250,210,'Notes',T(14,40,'# ideas')+T(14,58,'- milk drop')+T(14,76,'- Spleen font?'),false);
  h+=w(60,440,300,210,'Web',B(0,26,300,28,`background:${C.oat}`)+T(10,32,'milk.example',`color:${C.cocoa}`)+T(14,70,'Welcome',X2),false);
  h+=w(390,440,300,210,'Music',icon(20,44,80,'music',C.straw,C.esp)+T(116,56,'Cream Soda Skies')+T(116,76,'Lactose Intolerants',`color:${C.cocoa}`),false);
  h+=w(720,440,250,210,'Settings',T(14,40,'Display')+T(14,64,'Sound')+T(14,88,'Network')+T(14,112,'Keyboard'),false);
  h+=TC(0,718,1024,'Click a window to focus   Drag a window to another space   Esc to leave',`color:${C.cream}`);
  return h;
}

/* ---------- 08 Platinum (retro) ---------- */
function s08(){
  const dither=`repeating-conic-gradient(#E8DEC8 0% 25%, #FBF6EC 0% 50%) 0 0/4px 4px`;
  let h=B(0,0,1024,768,`background:${dither}`);
  const bar=`background:#FFFDF8;border-bottom:2px solid ${C.esp}`;
  h+=B(0,0,1024,24,bar)+T(14,4,'<b>&#9679;</b>  File   Edit   View   Special')+TR(1010,4,'10:41 AM');
  const stripes=`repeating-linear-gradient(0deg,${C.esp} 0 1px,#FFFDF8 1px 3px)`;
  const win=(x,y,w,hh,title,body)=>B(x,y,w,hh,`background:#FFFDF8;border:2px solid ${C.esp};box-shadow:3px 3px 0 ${C.esp}`,
     B(0,0,w-4,22,`background:${stripes};border-bottom:2px solid ${C.esp}`)+B(8,5,12,12,`background:#FFFDF8;border:2px solid ${C.esp}`)+
     B(w/2-title.length*4-14,3,title.length*8+28,16,'background:#FFFDF8')+TC(0,3,w-4,`<b>${title}</b>`)+body);
  h+=win(120,70,520,360,'Milk HD',
     [['folder','System'],['folder','Documents'],['term','Terminal'],['note','TeachText'],['folder','Games'],['gear','Control Panels'],['calc','Calculator'],['doc','ReadMe']].map((a,i)=>{
        const col=i%4,row=Math.floor(i/4),x=24+col*124,y=40+row*140;return icon(x+30,y,56,a[0],'#FFFDF8',C.esp,`border:2px solid ${C.esp};border-radius:6px`)+TC(x,y+66,116,a[1]);}).join('')+
     B(0,334,516,22,`background:#FFFDF8;border-top:2px solid ${C.esp}`)+T(10,338,'8 items        212 MB in disk        88 MB available'));
  h+=win(360,250,500,320,'SimpleText',
     T(14,36,'Dear future me,')+T(14,58,'The cow jumped over the moon,')+T(14,76,'and the moon was made of milk.')+T(14,112,'1. Draw rectangles')+T(14,130,'2. Draw windows')+T(14,148,'3. Draw a cow')+B(240,36,1,0,'')+B(14,168,8,16,`background:${C.esp}`)+
     B(480-4-16,22,16,262,`background:${C.oat};border-left:2px solid ${C.esp}`)+B(480-4-16,22,16,60,`background:#FFFDF8;border:2px solid ${C.esp}`));
  // desktop icons
  h+=icon(930,60,48,'gear','#FFFDF8',C.esp,`border:2px solid ${C.esp};border-radius:6px`)+TC(906,114,96,'Milk HD');
  h+=icon(930,640,48,'doc','#FFFDF8',C.esp,`border:2px solid ${C.esp};border-radius:6px`)+TC(906,694,96,'Trash');
  return h;
}

/* ---------- 09 Pantry (sidebar shell) ---------- */
function s09(){
  let h=B(0,0,1024,768,`background:linear-gradient(135deg,#F3E4CB,#E2CDAE)`);
  h+=B(0,0,1024,28,`background:${C.esp}`)+T(14,6,'Activities',`color:${C.cream};${BD}`)+TC(312,6,400,'Tue 20 Oct   10:41',`color:${C.cream}`)+TR(1010,6,'Wi-Fi   Vol   87%',`color:${C.sand}`);
  // dock left
  h+=B(0,28,72,740,`background:rgba(255,253,248,.88);border-right:1px solid ${C.sand}`);
  const d=[['folder',C.honey],['term',C.esp],['globe',C.blue],['note',C.milk],['music',C.straw],['photo',C.milk],['gear',C.latte]];
  d.forEach((k,i)=>{h+=icon(12,44+i*62,48,k[0],k[1],k[0]==='term'?C.cream:C.esp)+(i<2?B(2,44+i*62+20,4,8,`background:${C.straw};border-radius:2px`):'');});
  h+=icon(12,690,48,'calc',C.cream,C.esp,`border:1px solid ${C.sand}`);
  // files window
  h+=B(110,60,620,420,`background:${C.milk};border-radius:12px;border:1px solid ${C.sand};box-shadow:0 14px 34px rgba(90,64,40,.25);overflow:hidden`,
     B(0,0,620,40,`background:${C.cream};border-bottom:1px solid ${C.sand}`)+B(10,8,26,24,`background:${C.oat};border-radius:6px`)+TC(10,12,26,'&lt;')+B(40,8,26,24,`background:${C.oat};border-radius:6px`)+TC(40,12,26,'&gt;')+TC(120,12,380,'<b>Home / projects</b>')+TRi(620,606,12,'x',`color:${C.cocoa}`)+
     B(0,40,160,380,`background:${C.oat}55;border-right:1px solid ${C.sand}`)+T(16,56,'Home')+B(8,80,144,24,`background:${C.honey}55;border-radius:6px`)+T(16,84,'projects')+T(16,112,'school')+T(16,140,'Downloads')+T(16,168,'Pictures')+
     [['folder','kernel'],['folder','web'],['doc','notes.txt'],['photo','logo.png'],['folder','school'],['note','ideas.md']].map((a,i)=>{const col=i%3,row=Math.floor(i/3);const x=184+col*140,y=64+row*130;return icon(x+30,y,56,a[0],a[0]==='folder'?C.honey+'55':C.cream,a[0]==='folder'?C.cocoa:C.esp,a[0]==='folder'?'':`border:1px solid ${C.sand}`)+TC(x,y+66,116,a[1]);}).join(''));
  // shell window
  h+=B(380,330,560,330,`background:${C.esp};border-radius:12px;box-shadow:0 14px 34px rgba(60,40,30,.4);overflow:hidden`,
     B(0,0,560,40,`background:#4A3C34`)+TC(60,12,440,'<b>Shell</b>',`color:${C.cream}`)+TRi(560,546,12,'x',`color:${C.sand}`)+
     T(14,56,'$ ls projects',`color:${C.cream}`)+T(14,74,'kernel  web  notes.txt',`color:${C.matcha}`)+T(14,100,'$ git log --oneline -3',`color:${C.cream}`)+T(14,118,'a1b2c3d boot the framebuffer',`color:${C.latte}`)+T(14,136,'9f8e7d6 add UART output',`color:${C.latte}`)+T(14,162,'$ ',`color:${C.cream}`)+B(30,162,8,16,`background:${C.cream}`));
  // notification
  h+=B(740,44,264,66,`background:rgba(255,253,248,.95);border-radius:12px;border:1px solid ${C.sand};box-shadow:0 8px 20px rgba(90,64,40,.2)`,icon(10,12,40,'cal',C.honey+'66',C.esp)+T(60,12,'<b>Dentist</b>')+T(60,32,'in 15 minutes',`color:${C.cocoa}`));
  return h;
}

/* ---------- 10 Glass (alpha-layered) ---------- */
function s10(){
  let h=B(0,0,1024,768,`background:linear-gradient(150deg,#F7C9A8 0%,#E8A9B8 45%,#A99BD1 100%)`);
  h+=B(120,380,560,560,`border-radius:50%;background:rgba(255,253,248,.22)`)+B(640,-120,480,480,`border-radius:50%;background:rgba(255,230,160,.30)`);
  const g='background:rgba(255,253,248,.62);border:1px solid rgba(255,255,255,.75);';
  // island
  h+=B(332,12,360,40,`${g}border-radius:20px;box-shadow:0 6px 18px rgba(90,40,60,.2)`,drop(14,12,14)+TC(30,11,300,'<b>10:41</b>   Tue 20 Oct')+TRi(360,346,11,'87%'));
  // windows
  h+=B(80,100,540,360,`${g}border-radius:22px;box-shadow:0 22px 50px rgba(80,40,70,.3);overflow:hidden`,
     lights(18,16)+TC(120,14,300,'<b>Files</b>')+B(0,46,150,314,'background:rgba(255,253,248,.35)')+
     T(18,64,'Recents',BD)+T(18,92,'Projects')+T(18,120,'School')+T(18,148,'Pictures')+
     [['folder','kernel'],['folder','web'],['doc','notes'],['photo','bike']].map((a,i)=>{const col=i%2,row=Math.floor(i/2);const x=190+col*160,y=64+row*130;return icon(x+20,y,64,a[0],'rgba(255,253,248,.85)',C.esp)+TC(x,y+74,104,a[1]);}).join(''));
  h+=B(340,300,500,320,`background:rgba(58,46,40,.86);border:1px solid rgba(255,255,255,.35);border-radius:22px;box-shadow:0 22px 50px rgba(60,30,60,.4);overflow:hidden`,
     lights(18,16)+TC(120,14,260,'<b>Shell</b>',`color:${C.cream}`)+T(20,60,'$ milk status',`color:${C.cream}`)+T(20,80,'all good',`color:${C.matcha}`)+T(20,108,'$ ',`color:${C.cream}`)+B(36,108,8,16,`background:${C.cream}`));
  // control centre
  h+=B(724,72,280,288,`${g}border-radius:22px;box-shadow:0 18px 40px rgba(80,40,70,.25)`,
     B(16,16,118,56,'background:rgba(255,253,248,.7);border-radius:16px')+T(30,22,'<b>Wi-Fi</b>')+T(30,40,'home-5G',`color:${C.cocoa}`)+
     B(146,16,118,56,`background:${C.straw};border-radius:16px`)+T(160,22,'<b>Focus</b>',`color:${C.milk}`)+T(160,40,'On',`color:${C.milk}`)+
     B(16,88,248,56,'background:rgba(255,253,248,.7);border-radius:16px')+T(30,96,'Display',BD)+B(30,124,220,8,'background:rgba(122,102,82,.25);border-radius:4px',B(0,0,150,8,`background:${C.esp};border-radius:4px`))+
     B(16,160,248,56,'background:rgba(255,253,248,.7);border-radius:16px')+T(30,168,'Sound',BD)+B(30,196,220,8,'background:rgba(122,102,82,.25);border-radius:4px',B(0,0,90,8,`background:${C.esp};border-radius:4px`))+
     T(18,236,'Battery 87%',`color:${C.cocoa}`));
  // notification
  h+=B(724,380,280,72,`${g}border-radius:18px;box-shadow:0 10px 24px rgba(80,40,70,.22)`,icon(12,14,44,'cal',C.honey+'88',C.esp)+T(66,14,'<b>Dentist</b>')+T(66,34,'In 15 minutes',`color:${C.cocoa}`));
  // dock
  const d=['folder','term','globe','music','gear'];
  h+=B(332,688,360,68,`${g}border-radius:26px;box-shadow:0 12px 30px rgba(80,40,70,.25)`);
  d.forEach((k,i)=>{h+=icon(350+i*64,698,48,k,k==='term'?C.esp:'rgba(255,253,248,.9)',k==='term'?C.cream:C.esp);});
  return h;
}

/* ---------- data ---------- */
const CONCEPTS=[
 {n:'01',name:'Whole Milk',tag:'The macOS-style desktop',draw:s01,cost:'medium',
  idea:'Menu bar on top, floating dock at the bottom, rounded windows with traffic-light buttons. Cream and oat tones instead of grey. The "obvious" milkOS: Mac feel, milk skin.',
  why:'Windows need rounded-corner clipping, a shadow (a few alpha rects, no blur), and a dock with hover magnify only if you want it. A real window manager and an app menu model are the work.',
  els:['Menu bar','Dock','Windows + title bars','Finder-style file browser','Terminal','Spotlight (see 03)','Notifications','Lock screen'],
  font:'Unifont looks fine for menus and terminals but a rounded sans (like Inter or SF) would make the labels feel Mac. Dock labels especially.'},
 {n:'02',name:'Pail',tag:'The tiling, Linux-style desktop',draw:s02,cost:'easy',
  idea:'No floating windows. Every app fills a tile, a thin bar at the bottom shows workspaces, the focused tile gets a honey border. Keyboard first. Think sway/i3 in cream.',
  why:'Tiling is just rectangle maths: split, resize, redraw. No overlap means no z-order or shadows. This is the cheapest "real" shell and ideal for a hobby kernel.',
  els:['Status bar','Workspaces','Tiling layout engine','Focus border','Terminal','Keyboard shortcuts'],
  font:'Unifont is a perfect match: everything is a text grid. A mono font like Spleen or Terminus at 8x16 would look sharper.'},
 {n:'03',name:'Skim',tag:'Launcher-first shell',draw:s03,cost:'easy',
  idea:'The desktop is almost empty: a big clock and one launcher. Hit a key, type, press Enter. Apps, files and commands in the same box. Windows come and go as full-screen or half-screen.',
  why:'A text box, a result list, and a fuzzy matcher. The expensive piece is only the app and file index, and you need that for any desktop anyway.',
  els:['Launcher / command bar','Big clock','App + file index','Quick actions','Minimal status','Full-screen app windows'],
  font:'Unifont works. At 2x scale the launcher input is chunky and charming. A light sans would make it feel more like Spotlight.'},
 {n:'04',name:'Pour',tag:'Touch-friendly tablet shell',draw:s04,cost:'medium',
  idea:'Big rounded app tiles, a search pill, glanceable widgets, a swipe-up bar at the bottom. Everything is a thumb-size target (112px). Landscape tablet layout.',
  why:'Drawing is easy (big rounded squares). The cost is the input stack: touch events from virtio-input, gestures, momentum scroll, and an on-screen keyboard.',
  els:['Home grid','Widgets','Search pill','Gesture bar','On-screen keyboard','App switcher','Status bar'],
  font:'Unifont at 1x is too small for touch. Run it at 2x/3x or use a proper font at 20px or more.'},
 {n:'05',name:'Curd',tag:'Terminal-centric workspace',draw:s05,cost:'easy',
  idea:'The shell is the desktop. Panes like tmux with a tab strip on top, a command palette on Ctrl-P to launch anything. GUI apps open as floating overlays on top of panes.',
  why:'It is mostly text cells and borders, so redraws are cheap. The palette is a list and a filter. Closest to Linux roots.',
  els:['Tab strip','Panes','Command palette','Info panes (clock, disks, jobs)','Overlay windows','Keyboard-only flow'],
  font:'Unifont is the right font here: the whole UI lives on the 8x16 grid.'},
 {n:'06',name:'Today',tag:'Widget home screen',draw:s06,cost:'medium',
  idea:'The home screen is a glanceable dashboard of cards: calendar, next event, system health, notes, recent files. Apps are one click away from a small floating dock.',
  why:'Cards are rounded rects with text. The work is the widget model: each card needs its own small data source and redraw rule.',
  els:['Greeting + date','Card grid','Calendar','System meters','Notes','Recent files','Mini dock'],
  font:'This one wants a nicer font for the greeting and card titles. Unifont 3x headings will look blocky; try a clean sans for big text only.'},
 {n:'07',name:'Spaces',tag:'Mission-control style overview',draw:s07,cost:'hard',
  idea:'One key shows every window shrunk into a grid, plus a strip of virtual desktops on top. Pick a window, or drag it to another space.',
  why:'You need each window rendered into its own offscreen buffer and then scaled down. Scaled blits and animation timing are the hard parts.',
  els:['Window thumbnails','Spaces strip','Offscreen window buffers','Scaled blit','Drag between spaces','Dim overlay'],
  font:'Scaled-down Unifont turns into mush. Show titles under thumbnails in a normal-size font.'},
 {n:'08',name:'Platinum',tag:'Classic Mac OS in milk',draw:s08,cost:'easy',
  idea:'A love letter to System 7: dithered desktop, striped title bars, square corners, hard shadows. Cream and ink only. It looks right with chunky pixel text.',
  why:'Only rectangles, 1px borders and a dither pattern. No alpha, no rounded corners. Fastest way to get something that looks intentional.',
  els:['Menu bar','Striped title bars','Desktop icons','Trash','Dithered wallpaper','Hard drop shadows','Scroll bars'],
  font:'Unifont is already period-correct here (like Chicago). A real Chicago-style bitmap font would be the cherry on top.'},
 {n:'09',name:'Pantry',tag:'Sidebar-dock desktop',draw:s09,cost:'medium',
  idea:'GNOME / elementary style: a dark top panel, a vertical dock on the left, header-bar windows with back/forward buttons. A calmer layout for widescreen.',
  why:'Same engine as 01, plus a header-bar widget set (buttons, titles, close) and a top panel with a system tray and notification area.',
  els:['Top panel','Left dock','Header-bar windows','Notification toasts','Activities button','System tray'],
  font:'Unifont works. Header-bar titles in bold would want a real bold face.'},
 {n:'10',name:'Glass',tag:'Layered, translucent, floating',draw:s10,cost:'hard',
  idea:'Warm gradient wallpaper, translucent panels, a floating top "island", a control centre, generous rounding. Everything floats. The prettiest and the most work.',
  why:'Heavy alpha blending of big areas every frame plus shadows. Real background blur is out of reach, so panels fake it with higher opacity. Needs dirty rects and ideally a compositor.',
  els:['Floating top island','Translucent windows','Control centre','Notification stack','Floating dock','Gradient wallpaper'],
  font:'This is the concept that most needs a nicer font. Unifont on soft glass looks harsh; try a rounded sans at 14 to 16px.'}
];

export { CONCEPTS };

export { C, B, T, TC, TR, TRi, icon, lights, drop, X2, X3, X4, BD };
