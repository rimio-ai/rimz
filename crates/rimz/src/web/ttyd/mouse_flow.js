// ponytail: fixed pacing is validated at 179x38; scale by viewport if larger screens regress.
const MOTION_INTERVAL_MS=50;
const MOUSE_NONE=0;
const MOUSE_BOUNDARY=1;
const MOUSE_MOTION=2;
const XTERM_HELD_DRAG_EVENTS=6;
const DEBUG_MOUSE_FLOW=(()=>{
  try{
    return new URLSearchParams(window.location.search).get("rimzdebug")==="1";
  }catch(_){
    return false;
  }
})();
const mouseFlowDecisions=[];
const noteMouseFlow=action=>{
  if(!DEBUG_MOUSE_FLOW)return;
  mouseFlowDecisions.push({t:Date.now(),action});
  if(mouseFlowDecisions.length>256)mouseFlowDecisions.shift();
};
const mouseFlow={
  pending:null,
  timer:0,
  lastSentAt:null,
  suppressPress:false,
};
if(DEBUG_MOUSE_FLOW)window.__rimzWeb={flow:mouseFlow,decisions:mouseFlowDecisions};

const mouseReportKind=data=>{
  const bytes=data instanceof ArrayBuffer
    ?new Uint8Array(data)
    :ArrayBuffer.isView(data)
      ?new Uint8Array(data.buffer,data.byteOffset,data.byteLength)
      :null;
  if(!bytes||bytes.length<7||bytes[0]!==0x30||bytes[1]!==0x1b||bytes[2]!==0x5b)return MOUSE_NONE;
  if(bytes[3]===0x4d&&bytes.length===7){
    const code=bytes[4]-32;
    return code>=0&&(code&32)!==0?MOUSE_MOTION:MOUSE_BOUNDARY;
  }
  if(bytes[3]!==0x3c)return MOUSE_NONE;
  let index=4;
  let code=0;
  const codeStart=index;
  while(index<bytes.length&&bytes[index]>=0x30&&bytes[index]<=0x39){
    code=code*10+bytes[index]-0x30;
    index++;
  }
  if(index===codeStart||bytes[index++]!==0x3b)return MOUSE_NONE;
  for(let field=0;field<2;field++){
    const start=index;
    while(index<bytes.length&&bytes[index]>=0x30&&bytes[index]<=0x39)index++;
    if(index===start)return MOUSE_NONE;
    if(field===0&&bytes[index++]!==0x3b)return MOUSE_NONE;
  }
  if(index!==bytes.length-1)return MOUSE_NONE;
  const final=bytes[index];
  if(final!==0x4d&&final!==0x6d)return MOUSE_NONE;
  return final===0x4d&&(code&32)!==0?MOUSE_MOTION:MOUSE_BOUNDARY;
};

const armMouseFlowTimer=()=>{
  if(mouseFlow.timer)return;
  const elapsed=window.performance.now()-mouseFlow.lastSentAt;
  mouseFlow.timer=window.setTimeout(()=>{
    mouseFlow.timer=0;
    const pending=mouseFlow.pending;
    mouseFlow.pending=null;
    if(pending){
      noteMouseFlow("timer-send");
      mouseFlow.lastSentAt=window.performance.now();
      pending.send(pending.data);
    }
  },Math.max(MOTION_INTERVAL_MS-elapsed,0));
};
const flushPendingMouseMotion=action=>{
  const pending=mouseFlow.pending;
  mouseFlow.pending=null;
  if(pending){
    noteMouseFlow(action);
    mouseFlow.lastSentAt=window.performance.now();
    pending.send(pending.data);
  }
};
const sendWithMouseFlow=(send,data)=>{
  const kind=mouseReportKind(data);
  if(mouseFlow.suppressPress&&kind===MOUSE_BOUNDARY){
    noteMouseFlow("swallow-press");
    mouseFlow.suppressPress=false;
    return;
  }
  if(kind===MOUSE_MOTION){
    const now=window.performance.now();
    const ready=mouseFlow.lastSentAt===null
      ||now-mouseFlow.lastSentAt>=MOTION_INTERVAL_MS;
    if(ready&&!mouseFlow.timer){
      noteMouseFlow("send");
      mouseFlow.lastSentAt=now;
      send(data);
    }else{
      mouseFlow.pending={send,data};
      noteMouseFlow("coalesce");
      armMouseFlowTimer();
    }
    return;
  }
  window.clearTimeout(mouseFlow.timer);
  mouseFlow.timer=0;
  if(kind===MOUSE_BOUNDARY){
    flushPendingMouseMotion("boundary-flush");
    mouseFlow.lastSentAt=null;
  }else{
    flushPendingMouseMotion("input-flush");
  }
  send(data);
};

const resetMouseFlow=()=>{
  window.clearTimeout(mouseFlow.timer);
  mouseFlow.pending=null;
  mouseFlow.timer=0;
  mouseFlow.lastSentAt=null;
  mouseFlow.suppressPress=false;
};

const installMouseDragRearm=term=>{
  const service=term._core&&term._core.coreMouseService;
  const element=term.element;
  const ownerDocument=element&&element.ownerDocument;
  if(!service||typeof service.onProtocolChange!=="function"||!ownerDocument){
    noteMouseFlow("rearm-unavailable");
    return;
  }
  const drag={
    held:false,
    x:0,
    y:0,
    disabled:false,
    events:0,
    timer:0,
    shiftKey:false,
    altKey:false,
    ctrlKey:false,
    metaKey:false,
  };
  const remember=event=>{
    drag.x=event.clientX;
    drag.y=event.clientY;
    if(event.type==="mousedown"){
      if(event.button!==0||!element.contains(event.target))return;
      drag.held=true;
      drag.disabled=false;
      drag.shiftKey=event.shiftKey;
      drag.altKey=event.altKey;
      drag.ctrlKey=event.ctrlKey;
      drag.metaKey=event.metaKey;
    }else drag.held=(event.buttons&1)!==0;
  };
  for(const type of ["mousedown","mousemove","mouseup"]){
    ownerDocument.addEventListener(type,remember,{capture:true,passive:true});
  }
  service.onProtocolChange(events=>{
    drag.events=events;
    const reportsHeldDrag=(events&XTERM_HELD_DRAG_EVENTS)===XTERM_HELD_DRAG_EVENTS;
    if(!reportsHeldDrag){
      if(drag.held)drag.disabled=true;
      return;
    }
    if(!drag.held||!drag.disabled||drag.timer)return;
    drag.timer=window.setTimeout(()=>{
      drag.timer=0;
      if(!drag.held||!drag.disabled
        ||(drag.events&XTERM_HELD_DRAG_EVENTS)!==XTERM_HELD_DRAG_EVENTS)return;
      drag.disabled=false;
      mouseFlow.suppressPress=true;
      noteMouseFlow("rearm");
      try{
        element.dispatchEvent(new window.MouseEvent("mousedown",{
          bubbles:true,
          cancelable:true,
          view:window,
          button:0,
          buttons:1,
          clientX:drag.x,
          clientY:drag.y,
          shiftKey:drag.shiftKey,
          altKey:drag.altKey,
          ctrlKey:drag.ctrlKey,
          metaKey:drag.metaKey,
        }));
      }finally{
        mouseFlow.suppressPress=false;
      }
    },0);
  });
};
