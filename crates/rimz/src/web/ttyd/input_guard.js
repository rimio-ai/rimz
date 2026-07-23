const installInputGuard=(term,sendInput)=>{
  const altKeyChar=event=>{
    if(/^Key[A-Z]$/.test(event.code)){
      const ch=event.code.slice(3);
      return event.shiftKey?ch:ch.toLowerCase();
    }
    if(/^Digit[0-9]$/.test(event.code)&&!event.shiftKey)return event.code.slice(5);
    return null;
  };
  let altChordAt=-1e9;
  let suppressKeypress=false;
  const keyHandler=event=>{
    if(event.type==="keypress"&&suppressKeypress){
      suppressKeypress=false;
      event.preventDefault();
      event.stopPropagation();
      return false;
    }
    if(event.type==="keydown"){
      suppressKeypress=false;
      if(event.altKey&&!event.ctrlKey&&!event.metaKey){
        const ch=altKeyChar(event);
        if(ch&&sendInput("\u001b"+ch)){
          altChordAt=performance.now();
          suppressKeypress=true;
          event.preventDefault();
          event.stopPropagation();
          return false;
        }
      }
    }
    if(event.type!=="keydown"||event.key!=="Enter"||!event.shiftKey||event.altKey||event.ctrlKey||event.metaKey)return true;
    if(!sendInput("\u001b[13;2u"))return true;
    event.preventDefault();
    event.stopPropagation();
    return false;
  };
  const textarea=term.textarea;
  const root=term.element;
  if(!textarea||!root)return keyHandler;
  let swallowComposition=false;
  let releaseTimer=0;
  const release=()=>{
    window.clearTimeout(releaseTimer);
    releaseTimer=0;
    swallowComposition=false;
    textarea.value="";
  };
  const block=event=>{
    if(!swallowComposition||event.target!==textarea)return false;
    event.preventDefault();
    event.stopPropagation();
    return true;
  };
  root.addEventListener("compositionstart",event=>{
    if(event.target!==textarea||performance.now()-altChordAt>250)return;
    swallowComposition=true;
    window.clearTimeout(releaseTimer);
    releaseTimer=window.setTimeout(release,250);
    block(event);
    textarea.blur();
    textarea.focus();
  },true);
  for(const type of ["compositionupdate","beforeinput","input"]){
    root.addEventListener(type,block,true);
  }
  root.addEventListener("compositionend",event=>{
    if(!block(event))return;
    textarea.value="";
    window.queueMicrotask(release);
  },true);
  return keyHandler;
};
