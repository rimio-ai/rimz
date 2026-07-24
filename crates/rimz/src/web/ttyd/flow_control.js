const HIGH=128*1024;
const LOW=32*1024;
const BACKLOG_STALL_MS=500;
const MOUSE_STALL_MS=250;
const MOUSE_MIN_FRAME_MS=16;
const MOUSE_MAX_FRAME_MS=80;
const MOUSE_CELLS_PER_MS=256;
const MOUSE_NONE=0;
const MOUSE_BOUNDARY=1;
const MOUSE_MOTION=2;
const DEBUG=(()=>{
  try{
    return new URLSearchParams(window.location.search).get("rimzdebug")==="1";
  }catch(_){
    return false;
  }
})();
const debugDecisions=[];
const debugNote=action=>{
  if(!DEBUG)return;
  debugDecisions.push({t:Date.now(),action});
  if(debugDecisions.length>256)debugDecisions.shift();
};
const flow={
  bytes:0,
  waiters:[],
  mouse:{
    inFlight:false,
    outputParsed:false,
    outputPainted:false,
    paced:false,
    pending:null,
    paceTimer:0,
    stallTimer:0,
    frameMs:MOUSE_MIN_FRAME_MS,
  },
};
if(DEBUG)window.__rimzWeb={flow,decisions:debugDecisions};

const wakeFlowWaiters=()=>{
  const waiters=flow.waiters;
  flow.waiters=[];
  for(const wake of waiters)wake();
};
const resetBacklog=()=>{
  flow.bytes=0;
  wakeFlowWaiters();
};
const clearMouseFlight=()=>{
  window.clearTimeout(flow.mouse.paceTimer);
  window.clearTimeout(flow.mouse.stallTimer);
  flow.mouse.paceTimer=0;
  flow.mouse.stallTimer=0;
  flow.mouse.inFlight=false;
  flow.mouse.outputParsed=false;
  flow.mouse.outputPainted=false;
  flow.mouse.paced=false;
};
const resetMouseMotion=()=>{
  clearMouseFlight();
  flow.mouse.pending=null;
};
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
const releaseMouseMotion=()=>{
  clearMouseFlight();
  sendPendingMouseMotion();
};
const releaseReadyMouseMotion=()=>{
  if(
    flow.mouse.inFlight
    &&flow.mouse.outputParsed
    &&flow.mouse.outputPainted
    &&flow.mouse.paced
  ){
    debugNote("ready-release");
    releaseMouseMotion();
  }
};
const armMouseTimers=()=>{
  flow.mouse.paceTimer=window.setTimeout(()=>{
    flow.mouse.paceTimer=0;
    flow.mouse.paced=true;
    releaseReadyMouseMotion();
  },flow.mouse.frameMs);
  flow.mouse.stallTimer=window.setTimeout(()=>{
    debugNote("stall-release");
    releaseMouseMotion();
  },MOUSE_STALL_MS);
};
const sendMouseMotion=(send,data)=>{
  flow.mouse.inFlight=true;
  flow.mouse.outputParsed=false;
  flow.mouse.outputPainted=false;
  flow.mouse.paced=false;
  armMouseTimers();
  debugNote("sent");
  send(data);
};
const sendPendingMouseMotion=()=>{
  const pending=flow.mouse.pending;
  flow.mouse.pending=null;
  if(pending)sendMouseMotion(pending.send,pending.data);
};
const releasePaintedMouseMotion=()=>{
  if(!flow.mouse.inFlight||!flow.mouse.outputParsed)return;
  flow.mouse.outputPainted=true;
  releaseReadyMouseMotion();
};
const sendWithMouseFlow=(send,data)=>{
  const kind=mouseReportKind(data);
  if(kind===MOUSE_MOTION){
    if(flow.mouse.inFlight){
      flow.mouse.pending={send,data};
      debugNote("queued");
    }else{
      sendMouseMotion(send,data);
    }
    return;
  }
  if(kind===MOUSE_BOUNDARY){
    const pending=flow.mouse.pending;
    resetMouseMotion();
    if(pending){
      debugNote("boundary-flush");
      pending.send(pending.data);
    }
    send(data);
    return;
  }
  if(flow.mouse.pending){
    const pending=flow.mouse.pending;
    flow.mouse.pending=null;
    debugNote("input-flush");
    pending.send(pending.data);
  }
  send(data);
};
const waitForFlowDrain=()=>new Promise(resolve=>{
  let observed=flow.bytes;
  let timer=0;
  const wake=()=>{
    window.clearTimeout(timer);
    resolve();
  };
  const check=()=>{
    if(flow.bytes<LOW){
      wakeFlowWaiters();
    }else if(flow.bytes<observed){
      observed=flow.bytes;
      timer=window.setTimeout(check,BACKLOG_STALL_MS);
    }else{
      resetBacklog();
    }
  };
  flow.waiters.push(wake);
  timer=window.setTimeout(check,BACKLOG_STALL_MS);
});

const rewriteWsUrl=url=>{
  let target=url;
  try{
    const parsed=new URL(url,window.location.href);
    if(parsed.host===window.location.host&&parsed.pathname.endsWith("/ws")){
      const search=new URLSearchParams(window.location.search);
      if(search.has("room")){
        parsed.search="";
        parsed.searchParams.set("arg",search.get("room"));
      }else parsed.search=window.location.search;
      target=parsed;
    }
  }catch(_){}
  return target;
};

const NativeWebSocket=window.WebSocket;
const NativeWebSocketStream=window.WebSocketStream;
const installWebSocketGate=()=>{
  if(!NativeWebSocketStream){
    window.WebSocket=class extends NativeWebSocket{
      constructor(url,protocols){
        const target=rewriteWsUrl(url);
        if(protocols===undefined)super(target);
        else super(target,protocols);
        resetMouseMotion();
        this.addEventListener("close",resetMouseMotion,{once:true});
      }

      send(data){
        sendWithMouseFlow(payload=>NativeWebSocket.prototype.send.call(this,payload),data);
      }
    };
    return;
  }

  window.WebSocket=class extends EventTarget{
    static CONNECTING=0;
    static OPEN=1;
    static CLOSING=2;
    static CLOSED=3;
    CONNECTING=0;
    OPEN=1;
    CLOSING=2;
    CLOSED=3;

    constructor(url,protocols){
      super();
      this.readyState=this.CONNECTING;
      this.protocol="";
      this.writer=null;
      this.didOpen=false;
      this.pendingClose=null;
      resetMouseMotion();
      this.stream=new NativeWebSocketStream(rewriteWsUrl(url),{protocols});
      this.stream.closed.then(({closeCode,reason})=>{
        this.finishStreamClose(closeCode,reason,true);
      }).catch(()=>{
        // ttyd disables auto-reconnect on error, so an established drop closes directly.
        this.finishStreamClose(1006,"",false);
      });
      this.stream.opened.then(({readable,writable,protocol})=>{
        this.writer=writable.getWriter();
        const reader=readable.getReader();
        this.protocol=protocol;
        this.didOpen=true;
        this.readyState=this.OPEN;
        resetBacklog();
        resetMouseMotion();
        this.dispatchEvent(new Event("open"));
        this.read(reader);
        if(this.pendingClose){
          const {code,reason,wasClean}=this.pendingClose;
          this.dispatchClose(code,reason,wasClean);
        }
      }).catch(()=>{
        this.dispatchEvent(new Event("error"));
        this.dispatchClose(1006,"",false);
      });
    }

    set binaryType(_value){}

    send(data){
      if(!this.writer)return;
      sendWithMouseFlow(payload=>this.writer.write(payload).catch(()=>{}),data);
    }

    close(code,reason){
      if(this.readyState===this.CLOSING||this.readyState===this.CLOSED)return;
      this.stream.close({closeCode:code,reason});
      this.readyState=this.CLOSING;
    }

    async read(reader){
      try{
        while(true){
          const {value,done}=await reader.read();
          if(done)return;
          const data=typeof value==="string"
            ?value
            // Chromium 124 yields ArrayBuffer; newer builds can yield Uint8Array.
            :value instanceof ArrayBuffer
              ?value.slice(0)
              :value.buffer.slice(value.byteOffset,value.byteOffset+value.byteLength);
          this.dispatchEvent(new MessageEvent("message",{data}));
          while(flow.bytes>HIGH)await waitForFlowDrain();
        }
      }catch(_){}
    }

    finishStreamClose(code,reason,wasClean){
      if(this.didOpen)this.dispatchClose(code,reason,wasClean);
      else this.pendingClose={code,reason,wasClean};
    }

    dispatchClose(code,reason,wasClean){
      if(this.readyState===this.CLOSED)return;
      resetMouseMotion();
      this.readyState=this.CLOSED;
      this.dispatchEvent(new CloseEvent("close",{code,reason,wasClean}));
    }
  };
};

const installBacklogMeter=term=>{
  const updateMouseFrameMs=({cols=term.cols,rows=term.rows}={})=>{
    const cells=Math.max(1,cols*rows);
    flow.mouse.frameMs=Math.max(
      MOUSE_MIN_FRAME_MS,
      Math.min(MOUSE_MAX_FRAME_MS,Math.ceil(cells/MOUSE_CELLS_PER_MS)),
    );
  };
  updateMouseFrameMs();
  term.onResize(updateMouseFrameMs);
  const write=term.write.bind(term);
  term.write=(data,callback)=>{
    const length=data.length;
    flow.bytes+=length;
    write(data,()=>{
      flow.bytes=Math.max(flow.bytes-length,0);
      if(flow.mouse.inFlight){
        flow.mouse.outputParsed=true;
        flow.mouse.outputPainted=false;
      }
      if(flow.bytes<LOW)wakeFlowWaiters();
      if(typeof callback==="function")callback();
    });
  };
  term.onRender(releasePaintedMouseMotion);
};
