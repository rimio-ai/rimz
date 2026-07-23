const HIGH=512*1024;
const LOW=128*1024;
const STALL_MS=2000;
const flow={bytes:0,waiters:[]};

const wakeFlowWaiters=()=>{
  const waiters=flow.waiters;
  flow.waiters=[];
  for(const wake of waiters)wake();
};
const resetFlow=()=>{
  flow.bytes=0;
  wakeFlowWaiters();
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
      timer=window.setTimeout(check,STALL_MS);
    }else{
      resetFlow();
    }
  };
  flow.waiters.push(wake);
  timer=window.setTimeout(check,STALL_MS);
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
        resetFlow();
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
      this.writer.write(data).catch(()=>{});
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
      this.readyState=this.CLOSED;
      this.dispatchEvent(new CloseEvent("close",{code,reason,wasClean}));
    }
  };
};

const installBacklogMeter=term=>{
  const write=term.write.bind(term);
  term.write=(data,callback)=>{
    const length=data.length;
    flow.bytes+=length;
    write(data,()=>{
      flow.bytes=Math.max(flow.bytes-length,0);
      if(flow.bytes<LOW)wakeFlowWaiters();
      if(typeof callback==="function")callback();
    });
  };
};
