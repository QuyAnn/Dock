//! Original SmartDock widgets: shared live dock/gallery views and small interactive popups.
use std::cell::{Cell, RefCell};
use chrono::{Datelike, Local, Utc};
use windows::core::Result;
use super::dock::{RectF, Style, TileView};
use super::renderer::{Align, Renderer, TextStyle};
use super::tiles::DataState;
use crate::data::{calendar_progress, finance, productivity as p};
use crate::settings::{Settings, WorldCity};
use crate::win::widget_tools as tools;

const WHITE: u32 = 0xF4F6_FAFF;
const MUTED: u32 = 0xFFFF_FFA0;
const BLUE: u32 = 0x64B5_FFFF;
const GREEN: u32 = 0x62DA_A0FF;
const ORANGE: u32 = 0xFFBD_69FF;
fn rect(x:f32,y:f32,w:f32,h:f32) -> RectF { RectF {x,y,w,h} }
fn text(r:&mut Renderer,s:&str,rc:RectF,px:f32,color:u32) -> Result<()> { r.draw_text(s,rc,px,color,Align::Left) }
fn center(r:&mut Renderer,s:&str,rc:RectF,px:f32,color:u32) -> Result<()> { r.draw_text_styled(s,rc,px,color,Align::Center,TextStyle::Bold) }
fn inside(rc:RectF,x:f32,y:f32) -> bool { x>=rc.x && x<rc.x+rc.w && y>=rc.y && y<rc.y+rc.h }

pub fn width(id:&str) -> Option<f32> {
    if !crate::settings::WIDGET_IDS[22..].contains(&id) {return None;}
    super::widget_design::width(id)
}
pub fn has_popup(id:&str) -> bool {
    matches!(id,"stopwatch"|"notes"|"search"|"worldclock"|"brightness"|"recentshots"|"currency"|"screentime"|"sun"|"hourly"|"forecast"|"metric")
}
pub fn editable(id:&str) -> bool { matches!(id,"notes"|"search"|"worldclock"|"currency") }
pub fn view(id:&str,d:&DataState,s:&Settings) -> Option<TileView> {
    width(id)?;
    let now = Local::now();
    let mut v = match id {
        "pomodoro" => {
            let (rest,round,left,running) = p::pomodoro();
            TileView { progress:Some(1.0-left as f32 / if rest {300.0} else {1500.0}),
                accent:if rest {GREEN} else {ORANGE}, sub:if running {"▶"} else {"Ⅱ"}.into(),
                ..TileView::card(format!("{} · VÒNG {round}",if rest {"NGHỈ"} else {"TẬP TRUNG"}),crate::data::timer::time_text(left)) }
        }
        "stopwatch" => { let (elapsed,running,laps) = p::watch(); TileView {
            sub:format!("{} · {}",if running {"▶"} else {"Ⅱ"},laps.len()), ..TileView::card("BẤM GIỜ",p::watch_text(elapsed)) } }
        "notes" => TileView { style:Style::Note,sub:if s.note.is_empty() {"Bấm để viết ghi chú".into()} else {s.note.replace(['\r','\n']," ")},..TileView::card("Sổ ghi chú","") },
        "dayprog"|"monthprog"|"yearprog" => {
            let (day,month,year,days) = calendar_progress::fractions(now.naive_local());
            let (label,frac) = match id { "dayprog"=>("HÔM NAY",day),"monthprog"=>("THÁNG NÀY",month),_=>("NĂM NAY",year) };
            TileView { progress:Some(frac),accent:BLUE, cells:if id=="yearprog" {(0..days).map(|i| u8::from(i<now.ordinal0())).collect()} else {Vec::new()},
                ..TileView::card(label,format!("{:.1}%",frac*100.0)) }
        }
        "theme" => TileView::card("GIAO DIỆN",if tools::light_theme().unwrap_or(true) {"☀ Sáng"} else {"☾ Tối"}),
        "brightness" => TileView { progress:tools::brightness().map(|n| n as f32/100.0),accent:ORANGE,
            ..TileView::card("ĐỘ SÁNG",tools::brightness().map_or("Không hỗ trợ".into(),|n|format!("{n}%"))) },
        "recyclebin" => { let b=crate::win::shellinfo::info(); TileView {style:Style::Metrics,sub:crate::data::widgets::size_text(b.bin_bytes),..TileView::card("THÙNG RÁC",format!("{} mục",b.bin_items))} }
        "screenshot" => TileView::card("CHỤP MÀN HÌNH","✂  Chụp vùng"),
        "recentshots" => TileView::card("ẢNH CHỤP GẦN ĐÂY",if tools::shots().is_empty() {"Chưa có ảnh"} else {""}),
        "search" => TileView::card("TÌM KIẾM","Web / thư mục…"),
        "worldclock" => TileView::card("GIỜ THẾ GIỚI",""),
        "analog"|"flipclock"|"segclock" => TileView::card("",now.format("%H:%M").to_string()),
        "daynum" => TileView::card(now.format("%m / %Y").to_string(),now.day().to_string()),
        "sun" => { let value = d.weather.as_ref().and_then(|w| Some(format!("{} ↑  {} ↓",weather_time(w.sunrise?,w),weather_time(w.sunset?,w)))).unwrap_or("Chưa có dữ liệu".into()); TileView::card("MẶT TRỜI",value) }
        "hourly"|"forecast" => TileView::card(if id=="hourly" {"6 GIỜ TỚI"} else {"4 NGÀY TỚI"},if d.weather.is_some() {""} else {"Chưa có dữ liệu"}),
        "metric" => { let f=finance::snapshot(); TileView {
            detail:f.btc_change.map_or("—".into(),|n|format!("{} {:.2}%",if n>=0.0 {"▲"}else{"▼"},n.abs())),
            accent:if f.btc_change.is_some_and(|n|n<0.0) {0xFF45_3AFF}else{0x29CD_50FF},
            sub:if f.btc_error {"Cũ"} else {"USD"}.into(),..TileView::card("BITCOIN",f.btc.map_or(if f.btc_error {"Lỗi mạng"} else {"Đang tải…"}.into(),|n|format!("${n:.0}")))} }
        "currency" => { let f=finance::snapshot(); TileView {style:Style::Metrics,
            sub:finance::convert(&f.rates,s.currency_amount,&s.currency_from,&s.currency_to).map_or("Chưa có tỷ giá".into(),|n|format!("{n:.2} {}{}",s.currency_to,if f.rates_error {" · cũ"} else {""})),
            ..TileView::card("ĐỔI TIỀN",format!("{} {}",s.currency_amount,s.currency_from))} }
        "screentime" => { let rows=crate::data::screentime::snapshot().ranked(); let secs=rows.iter().map(|(_,v)|v).sum::<f64>();
            TileView::card("MÀN HÌNH HÔM NAY",duration_text(secs)) }
        _ => return None,
    };
    if matches!(id,"pomodoro"|"yearprog"|"worldclock"|"analog"|"flipclock"|"segclock"|"daynum"|"sun"|"hourly"|"forecast"|"recentshots") {
        v.style=Style::Widget;
    }
    v.extra=id.into(); v.design_id=id.into(); Some(v)
}
fn duration_text(secs:f64) -> String { format!("{} giờ {} phút",secs as u64/3600,secs as u64/60%60) }
fn weather_time(at:i64,w:&crate::data::weather::Weather) -> String {
    chrono::DateTime::from_timestamp(at+w.utc_offset_seconds as i64,0).map_or("--:--".into(),|d|d.format("%H:%M").to_string())
}

/// Clock/forecast geometry is drawn from live data attached to the view. Never fetch in paint.
pub fn enrich(v:&mut TileView,d:&DataState,s:&Settings) {
    match v.extra.as_str() {
        "worldclock" => { v.rows=s.world_cities.iter().take(3).map(|city| {
            let t=calendar_progress::city_time(Utc::now(),city.offset_minutes);
            super::dock::UsageRow {label:city.name.clone(),value:t.format("%H:%M").to_string(),right:t.format("%d/%m").to_string(),frac:0.0,color:BLUE}
        }).collect(); }
        "hourly"|"forecast" => {
            if let Some(w)=&d.weather {
                let mut rows=Vec::new();
                if v.extra=="hourly" {
                    for h in w.hourly.iter().filter(|h|h.at>=Utc::now().timestamp()).take(6) {
                        rows.push(super::dock::UsageRow {label:weather_time(h.at,w),value:format!("{:.0}°",h.temp),right:h.code.to_string(),frac:0.0,color:BLUE});
                    }
                } else {
                    let local_date=chrono::DateTime::from_timestamp(Utc::now().timestamp()+w.utc_offset_seconds as i64,0).unwrap().date_naive();
                    for day in w.daily.iter().filter(|day|chrono::NaiveDate::parse_from_str(&day.date,"%Y-%m-%d").is_ok_and(|d|d>local_date)).take(4) {
                        rows.push(super::dock::UsageRow {label:chrono::NaiveDate::parse_from_str(&day.date,"%Y-%m-%d").map_or(day.date.clone(),|d|d.format("%a").to_string()),value:format!("{:.0}°/{:.0}°",day.max,day.min),right:day.code.to_string(),frac:0.0,color:BLUE});
                    }
                }
                v.rows=rows;
                if v.rows.is_empty() { v.value="Chờ cập nhật".into(); }
            }
        }
        "screentime" => {v.series=vec![crate::data::screentime::snapshot().ranked().iter().take(6).map(|(_,v)|*v as f32).collect()];}
        "sun" => {
            v.progress=d.weather.as_ref().and_then(|w| {
                let (rise,set)=(w.sunrise?,w.sunset?);
                (set>rise).then_some(((Utc::now().timestamp()-rise) as f32/(set-rise) as f32).clamp(0.0,1.0))
            });
        }
        _=>{}
    }
}

pub fn draw(r:&mut Renderer,t:&TileView,rc:RectF,hover:bool) -> Result<()> {
    super::widget_design::draw(r,t,rc,hover)
}
fn draw_shot(r:&mut Renderer,shot:&tools::Shot,a:RectF) -> Result<()> {
    r.draw_bitmap_rounded(shot.w,shot.h,&shot.rgba,a,7.0)
}

#[derive(Debug,PartialEq)]
pub enum Action { SaveNote(String), SetCities(Vec<WorldCity>), SetCurrency(f64,String,String), Search(String),
    WatchToggle, WatchReset, Lap, OpenShot(usize), None }
thread_local! {
    static INPUT:RefCell<String>=const {RefCell::new(String::new())};
    static ERROR:RefCell<String>=const {RefCell::new(String::new())};
    static SELECTED:Cell<bool>=const {Cell::new(false)};
    static LAP_PAGE:Cell<usize>=const {Cell::new(0)};
    static BRIGHT_DRAG:Cell<bool>=const {Cell::new(false)};
    static BRIGHT_VALUE:Cell<Option<u32>>=const {Cell::new(None)};
    static BRIGHT_AT:Cell<Option<std::time::Instant>>=const {Cell::new(None)};
}
pub fn prepare(id:&str,s:&Settings) {
    INPUT.with(|v|*v.borrow_mut()=match id {
        "notes"=>s.note.clone(),"worldclock"=>s.world_cities.iter().take(3).map(|c|format!("{}={}",c.name,c.offset_minutes as f64/60.0)).collect::<Vec<_>>().join("; "),
        "currency"=>format!("{} {} {}",s.currency_amount,s.currency_from,s.currency_to),_=>String::new()});
    ERROR.with(|v|v.borrow_mut().clear()); SELECTED.with(|v|v.set(false)); LAP_PAGE.with(|v|v.set(0));
    BRIGHT_DRAG.with(|v|v.set(false)); BRIGHT_VALUE.with(|v|v.set(None));
    BRIGHT_AT.with(|v|v.set(None));
}
pub fn parse_cities(input:&str) -> Option<Vec<WorldCity>> {
    let cities:Option<Vec<_>>=input.split(';').map(|part| {
        let (name,offset)=part.trim().split_once('=')?; let offset:f64=offset.trim().parse().ok()?;
        let name=name.trim();
        if name.is_empty() || name.chars().count()>24 || !offset.is_finite() || !(-12.0..=14.0).contains(&offset) {return None;}
        Some(WorldCity::new(name,(offset*60.0).round() as i32))
    }).collect();
    cities.filter(|v|!v.is_empty() && v.len()<=3)
}
pub fn parse_currency(input:&str) -> Option<(f64,String,String)> {
    let mut parts=input.split_whitespace();
    let amount:f64=parts.next()?.parse().ok()?;
    let from=parts.next()?.to_ascii_uppercase(); let to=parts.next()?.to_ascii_uppercase();
    if parts.next().is_some() || !amount.is_finite() || amount<0.0 || amount>1e15 ||
        [&from,&to].iter().any(|s|s.len()!=3 || !s.bytes().all(|b|b.is_ascii_uppercase())) {return None;}
    Some((amount,from,to))
}
fn submit(id:&str) -> Action {
    let input=INPUT.with(|v|v.borrow().clone());
    let action=match id {
        "notes"=>Some(Action::SaveNote(input.trim().into())),
        "search"=>(!input.trim().is_empty()).then_some(Action::Search(input.trim().into())),
        "worldclock"=>parse_cities(&input).map(Action::SetCities),
        "currency"=>parse_currency(&input).and_then(|(amount,from,to)| {
            let f=finance::snapshot();
            (f.rates.is_empty() || (f.rates.contains_key(&from)&&f.rates.contains_key(&to))).then_some(Action::SetCurrency(amount,from,to))
        }),_=>None,
    };
    if let Some(a)=action {ERROR.with(|e|e.borrow_mut().clear());a} else {
        ERROR.with(|e|*e.borrow_mut()=match id {"worldclock"=>"Dùng Tên=UTC giờ; tối đa 3 thành phố (-12…14)","currency"=>"Dùng số tiền và 2 mã tiền hợp lệ, ví dụ: 100 USD VND",_=>"Nhập nội dung trước khi tiếp tục"}.into()); Action::None
    }
}
pub fn input(id:&str,ch:char) -> Action {
    if !editable(id) {return Action::None;}
    if id=="notes" && ch=='\r' && unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x10) } < 0 {
        INPUT.with(|v| {let mut v=v.borrow_mut();if v.chars().count()<1000 {v.push('\n');}});
        return Action::None;
    }
    if matches!(ch,'\r'|'\n') {return submit(id);}
    INPUT.with(|v| {
        let mut v=v.borrow_mut();
        match ch {
            '\u{1}'=>SELECTED.with(|s|s.set(true)),
            '\u{8}'=> {if SELECTED.with(|s|s.replace(false)) {v.clear();} else {v.pop();}},
            '\u{16}'=> {if let Some(clip)=crate::win::shellinfo::clipboard_text_full() {
                if SELECTED.with(|s|s.replace(false)) {v.clear();}
                let clip=if id=="notes" {clip.replace('\r',"")} else {clip.replace(['\r','\n']," ")};
                let available=1000usize.saturating_sub(v.chars().count());v.extend(clip.chars().filter(|c|!c.is_control() || *c=='\n').take(available));
            }},
            c if !c.is_control()=>{if SELECTED.with(|s|s.replace(false)) {v.clear();} if v.chars().count()<1000 {v.push(c);}},_=>{}
        }
    }); Action::None
}
fn button(r:&mut Renderer,label:&str,a:RectF) -> Result<()> {
    r.fill_rounded_rect(a,8.0,0xFFFF_FF20)?;center(r,label,a,11.0,WHITE)
}
pub fn popup_height(id:&str) -> f32 {match id {"stopwatch"=>426.0,"recentshots"=>430.0,"screentime"=>418.0,"notes"=>260.0,"currency"=>304.0,_=>240.0}}
pub fn paint_popup(r:&mut Renderer,id:&str,rc:RectF,d:&DataState,s:&Settings) -> Result<()> {
    let (x,y,w)=(rc.x+16.0,rc.y+12.0,rc.w-32.0);
    text(r,crate::settings::widget_label(id),rect(x,y,w,26.0),17.0,WHITE)?;
    if editable(id) {
        let hint=match id {"notes"=>"Enter: lưu · Shift+Enter: xuống dòng · Ctrl+A / Ctrl+V", "search"=>"Nhập từ khóa hoặc đường dẫn thư mục · Enter", "worldclock"=>"Tên=UTC giờ; tối đa 3 thành phố · offset cố định",_=>"Số tiền MÃ_NGUỒN MÃ_ĐÍCH · ví dụ: 100 USD VND"};
        text(r,hint,rect(x,y+32.0,w,22.0),9.0,MUTED)?;
        let input=rect(x,y+62.0,w,if id=="notes" {96.0} else {54.0});
        r.fill_rounded_rect(input,10.0,if SELECTED.with(Cell::get) {0x398D_DD60} else {0x0000_0045})?;
        let typed=INPUT.with(|v|v.borrow().clone());
        let max_chars=(w/6.5).max(1.0) as usize;
        let mut visual_lines=Vec::new();
        for line in typed.split('\n') {
            let chars:Vec<char>=line.chars().collect();
            if chars.is_empty() {visual_lines.push(String::new());}
            else {visual_lines.extend(chars.chunks(max_chars).map(|chunk|chunk.iter().collect::<String>()));}
        }
        if let Some(last)=visual_lines.last_mut() {last.push('|');}
        let count=if id=="notes" {4} else {2};
        for (i,line) in visual_lines[visual_lines.len().saturating_sub(count)..].iter().enumerate() {
            text(r,line,rect(input.x+9.0,input.y+5.0+i as f32*19.0,w-18.0,19.0),11.0,WHITE)?;
        }
        let by=if id=="notes" {y+174.0} else {y+134.0};
        button(r,if id=="search" {"Tìm / mở thư mục"} else {"Lưu"},rect(x,by,w,30.0))?;
        let err=ERROR.with(|v|v.borrow().clone());text(r,&err,rect(x,by+33.0,w,24.0),9.0,ORANGE)?;
        if id=="currency" {
            let f=finance::snapshot();
            if let Some((amount,from,to))=parse_currency(&typed) {
                let result=finance::convert(&f.rates,amount,&from,&to).map_or("Chưa có tỷ giá".into(),|n|format!("{amount} {from} = {n:.2} {to}"));
                text(r,&result,rect(x,y+192.0,w,24.0),12.0,WHITE)?;
            }
            text(r,"Tỷ giá: exchangerate-api.com (Open Access)",rect(x,y+224.0,w,20.0),9.0,BLUE)?;
            let stamp=f.rates_at.map_or("Chưa tải thành công".into(),|at|format!("{}{}",at.with_timezone(&Local).format("%d/%m %H:%M"),if f.rates_error {" · Lỗi mạng, tỷ giá cũ"} else {" · làm mới mỗi giờ"}));
            text(r,&stamp,rect(x,y+248.0,w,22.0),9.0,MUTED)?;
        }
        return Ok(());
    }
    match id {
        "stopwatch"=>{
            let (elapsed,running,laps)=p::watch();
            center(r,&p::watch_text(elapsed),rect(x,y+38.0,w,36.0),27.0,WHITE)?;
            let bw=(w-12.0)/3.0;
            for (i,label) in [if running {"Tạm dừng"} else {"Bắt đầu"},"Ghi vòng","Đặt lại"].iter().enumerate() {button(r,label,rect(x+i as f32*(bw+6.0),y+84.0,bw,30.0))?;}
            text(r,"Chuột phải trên thanh: ghi vòng và mở danh sách",rect(x,y+120.0,w,20.0),9.0,MUTED)?;
            let pages=laps.len().div_ceil(8).max(1);let page=LAP_PAGE.with(|v|v.get().min(pages-1));
            let start=laps.len().saturating_sub((page+1)*8);let end=laps.len().saturating_sub(page*8);
            for (row,i) in (start..end).rev().enumerate() {
                let split=laps[i]-if i==0 {std::time::Duration::ZERO} else {laps[i-1]};
                text(r,&format!("#{:02}   {}     +{}",i+1,p::watch_text(laps[i]),p::watch_text(split)),rect(x,y+146.0+row as f32*23.0,w,22.0),11.0,WHITE)?;
            }
            if laps.is_empty() {text(r,"Chưa có vòng · tối đa 100 vòng",rect(x,y+150.0,w,24.0),11.0,MUTED)?;}
            button(r,&format!("‹   Trang {} / {}   ›",page+1,pages),rect(x,y+364.0,w,28.0))?;
        }
        "brightness"=>{
            let actual=tools::brightness();
            if BRIGHT_VALUE.with(Cell::get)==actual || BRIGHT_AT.with(|v|v.get().is_some_and(|at|at.elapsed().as_secs()>10)) {
                BRIGHT_VALUE.with(|v|v.set(None));
            }
            let value=actual.map(|v|BRIGHT_VALUE.with(Cell::get).unwrap_or(v));
            if let Some(value)=value {
                center(r,&format!("{value}%"),rect(x,y+54.0,w,36.0),28.0,WHITE)?;
                let track=rect(x+8.0,y+122.0,w-16.0,6.0);
                r.fill_rounded_rect(track,3.0,0xFFFF_FF33)?;
                r.fill_rounded_rect(RectF{w:track.w*value as f32/100.0,..track},3.0,ORANGE)?;
                r.fill_rounded_rect(rect(track.x+track.w*value as f32/100.0-7.0,track.y-4.0,14.0,14.0),7.0,WHITE)?;
                text(r,"Kéo thanh để chỉnh độ sáng màn hình tích hợp",rect(x,y+150.0,w,22.0),10.0,MUTED)?;
            } else {text(r,"Màn hình không hỗ trợ WMI Brightness.",rect(x,y+75.0,w,30.0),12.0,MUTED)?;}
        }
        "recentshots"=>{
            let shots=tools::shots();
            for (i,shot) in shots.iter().enumerate() {
                let a=rect(x,y+42.0+i as f32*116.0,110.0,100.0);draw_shot(r,shot,a)?;
                text(r,&shot.path.file_name().unwrap_or_default().to_string_lossy(),rect(x+120.0,a.y,w-120.0,46.0),10.0,WHITE)?;
                text(r,"Bấm để mở",rect(x+120.0,a.y+48.0,w-120.0,22.0),10.0,BLUE)?;
            }
            if shots.is_empty() {text(r,"Chưa có ảnh trong Pictures\\Screenshots",rect(x,y+60.0,w,50.0),11.0,MUTED)?;}
        }
        "screentime"=>{
            let rows=crate::data::screentime::snapshot().ranked();
            text(r,"Hôm nay · bỏ qua thời gian ngủ / không thao tác >5 phút",rect(x,y+32.0,w,22.0),8.5,MUTED)?;
            for (i,(exe,secs)) in rows.iter().take(9).enumerate() {
                let name=std::path::Path::new(exe).file_stem().unwrap_or_default().to_string_lossy();
                text(r,&name,rect(x,y+66.0+i as f32*35.0,w*0.53,24.0),11.0,WHITE)?;
                text(r,&duration_text(*secs),rect(x+w*0.54,y+66.0+i as f32*35.0,w*0.46,24.0),10.0,BLUE)?;
            }
            if rows.is_empty() {text(r,"Bật widget để bắt đầu theo dõi cục bộ.",rect(x,y+80.0,w,26.0),11.0,MUTED)?;}
        }
        "metric"=>{
            let f=finance::snapshot();
            center(r,&f.btc.map_or("Chưa có dữ liệu".into(),|n|format!("${n:.2}")),rect(x,y+48.0,w,42.0),28.0,WHITE)?;
            text(r,"BTC / USD · Coinbase · cập nhật mỗi 60 giây",rect(x,y+104.0,w,26.0),10.0,MUTED)?;
            let stamp=f.btc_at.map_or("Chưa tải thành công".into(),|t|format!("Lần cuối: {}",t.with_timezone(&Local).format("%H:%M:%S")));
            text(r,&stamp,rect(x,y+136.0,w,24.0),10.0,MUTED)?;
            if f.btc_error {text(r,"Lỗi mạng · đang giữ giá cũ",rect(x,y+164.0,w,24.0),10.0,ORANGE)?;}
            else {text(r,"% thay đổi so với lần tải giá thành công trước.",rect(x,y+164.0,w,24.0),10.0,MUTED)?;}
        }
        "sun"|"hourly"|"forecast"=>{
            if let Some(v)=view(id,d,s) { let mut v=v;enrich(&mut v,d,s);draw(r,&v,rect(x,y+50.0,w,64.0),false)?; }
            if let Some(wd)=&d.weather {
                text(r,&format!("{} · Open-Meteo",wd.location),rect(x,y+132.0,w,24.0),11.0,WHITE)?;
                text(r,&format!("Cập nhật {} · giờ địa phương của địa điểm",wd.fetched_at.with_timezone(&Local).format("%H:%M")),rect(x,y+159.0,w,26.0),9.0,MUTED)?;
            }
        }
        _=>{}
    }
    Ok(())
}
pub fn click(id:&str,x:f32,y:f32,rc:RectF) -> Action {
    let (lx,ly,w)=(x-rc.x-16.0,y-rc.y-12.0,rc.w-32.0);
    if editable(id) {
        let by=if id=="notes" {174.0} else {134.0};
        if inside(rect(0.0,by,w,30.0),lx,ly) {return submit(id);} return Action::None;
    }
    match id {
        "stopwatch"=>{
            if inside(rect(0.0,84.0,w,30.0),lx,ly) {
                return match (lx/((w+6.0)/3.0)) as usize {0=>Action::WatchToggle,1=>Action::Lap,_=>Action::WatchReset};
            }
            if inside(rect(0.0,364.0,w,28.0),lx,ly) {
                let pages=p::watch().2.len().div_ceil(8).max(1);
                LAP_PAGE.with(|v|v.set(if lx<w/2.0 {v.get().saturating_sub(1)} else {(v.get()+1).min(pages-1)}));
            }
        }
        "recentshots"=>{if ly>=42.0 && ly<390.0 && lx>=0.0 && lx<w {return Action::OpenShot(((ly-42.0)/116.0) as usize);}},
        _=>{}
    }
    Action::None
}
pub fn brightness_drag(phase:super::popup::DragPhase,x:f32,y:f32,rc:RectF) -> bool {
    use super::popup::DragPhase;
    match phase {
        DragPhase::Down=>{
            let on=tools::brightness().is_some() && inside(rect(rc.x+16.0,rc.y+114.0,rc.w-32.0,44.0),x,y);
            BRIGHT_DRAG.with(|v|v.set(on));if !on {return false;}
        }
        _=>if !BRIGHT_DRAG.with(Cell::get) {return false;},
    }
    let n=(((x-rc.x-24.0)/(rc.w-48.0))*100.0).clamp(0.0,100.0).round() as u32;
    BRIGHT_VALUE.with(|v|v.set(Some(n)));tools::request_brightness(n);
    BRIGHT_AT.with(|v|v.set(Some(std::time::Instant::now())));
    if phase==DragPhase::Up {BRIGHT_DRAG.with(|v|v.set(false));}
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise the real D2D/DirectWrite paths without showing a window or touching AppBars.
    #[test]
    fn renders_new_tiles_and_popups_to_a_hidden_window() {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::*;
        struct TestWindow(windows::Win32::Foundation::HWND);
        impl Drop for TestWindow { fn drop(&mut self) { unsafe { let _=DestroyWindow(self.0); } } }
        let hwnd=unsafe {CreateWindowExW(WS_EX_TOOLWINDOW,w!("STATIC"),w!("SmartDock widget render test"),
            WS_POPUP,0,0,1000,800,None,None,None,None)}.unwrap();
        let _window=TestWindow(hwnd);
        let mut renderer=Renderer::new(hwnd,1000,800,96);
        let now=Utc::now();
        let weather=crate::data::weather::Weather {
            location:"Hà Nội".into(),
            current:crate::data::weather::Current{temp:28.0,feels:30.0,code:0,humidity:60.0,wind:8.0},
            daily:(0..5).map(|i|crate::data::weather::Day{date:(now.date_naive()+chrono::Duration::days(i)).to_string(),min:23.0,max:30.0,code:2}).collect(),
            hourly:(1..=6).map(|i|crate::data::weather::Hour{at:now.timestamp()+i*3600,temp:27.0,code:3}).collect(),
            sunrise:Some(now.timestamp()-3600),sunset:Some(now.timestamp()+3600),utc_offset_seconds:0,fetched_at:now,
        };
        let d=DataState{weather:Some(weather),..DataState::default()};
        let s=Settings{note:"Ghi chú tiếng Việt 📝\nDòng thứ hai".into(),..Settings::default()};
        for id in &crate::settings::WIDGET_IDS[22..] {
            renderer.begin().unwrap();
            let mut v=view(id,&d,&s).unwrap();enrich(&mut v,&d,&s);
            for (i,factor) in [1.0,0.86].iter().enumerate() {
                super::super::dock::draw_tile(&mut renderer,&v,rect(10.0,10.0+i as f32*60.0,width(id).unwrap()*factor,42.0),false).unwrap();
            }
            if has_popup(id) {
                prepare(id,&s);
                paint_popup(&mut renderer,id,rect(350.0,10.0,320.0,popup_height(id)),&d,&s).unwrap();
            }
            let shot=tools::Shot{path:std::path::PathBuf::new(),w:2,h:2,rgba:std::sync::Arc::new(vec![255;16])};
            draw_shot(&mut renderer,&shot,rect(10.0,150.0,90.0,60.0)).unwrap();
            renderer.end().unwrap();
        }
    }
    #[test]
    fn configuration_validates_offsets_names_and_currency() {
        let c=parse_cities("Tokyo=9; Delhi=5.5; New York=-5").unwrap();assert_eq!(c[1].offset_minutes,330);
        for bad in ["A=NaN","A=15","=0","A=0;B=1;C=2;D=3",""] {assert!(parse_cities(bad).is_none(),"{bad}");}
        assert_eq!(parse_currency("100 usd vnd"),Some((100.0,"USD".into(),"VND".into())));
        for bad in ["-1 USD VND","NaN USD VND","1 USD VND EUR","1 US VND"] {assert!(parse_currency(bad).is_none());}
    }
    #[test]
    fn editors_preserve_unicode_backspace_and_replace_all() {
        let mut s=Settings::default();s.note="Ghi chú 📝".into();
        prepare("notes",&s);
        input("notes",'\u{8}'); input("notes",'m');
        assert_eq!(submit("notes"),Action::SaveNote("Ghi chú m".into()));
        input("notes",'\u{1}');input("notes",'M');input("notes",'ớ');input("notes",'i');
        assert_eq!(submit("notes"),Action::SaveNote("Mới".into()));
        prepare("worldclock",&s);
        assert_eq!(submit("worldclock"),Action::SetCities(s.world_cities));
        prepare("search",&Settings::default());
        assert_eq!(submit("search"),Action::None);
        input("search",'a');assert_eq!(submit("search"),Action::Search("a".into()));
    }
    #[test]
    fn every_new_widget_has_a_live_view() {
        let d=DataState::default();let s=Settings::default();
        for id in crate::settings::WIDGET_IDS {
            if width(id).is_some() {let mut v=view(id,&d,&s).unwrap();enrich(&mut v,&d,&s);assert_eq!(v.extra,id);}
        }
    }
}
