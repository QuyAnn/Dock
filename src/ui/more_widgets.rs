//! Original SmartDock widgets: shared live dock/gallery views and small interactive popups.
use std::cell::{Cell, RefCell};
use chrono::{Datelike, Local, Timelike, Utc};
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
    Some(match id {
        "pomodoro" => 148.0, "stopwatch" => 120.0, "notes" => 170.0,
        "dayprog" | "monthprog" => 110.0, "yearprog" => 190.0,
        "theme" => 94.0, "brightness" => 114.0, "recyclebin" => 142.0,
        "screenshot" => 104.0, "recentshots" => 158.0, "search" => 138.0,
        "worldclock" => 222.0, "analog" => 48.0, "flipclock" => 104.0,
        "segclock" => 118.0, "daynum" => 54.0, "sun" => 148.0,
        "hourly" => 234.0, "forecast" => 206.0, "metric" => 138.0,
        "currency" => 158.0, "screentime" => 144.0, _ => return None,
    })
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
        "stopwatch" => { let (elapsed,running,lap_count) = p::watch_summary(); TileView {
            sub:format!("{} · {}",if running {"▶"} else {"Ⅱ"},lap_count), ..TileView::card("BẤM GIỜ",p::watch_text(elapsed)) } }
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
        "recentshots" => TileView::card("ẢNH CHỤP GẦN ĐÂY",if !tools::has_shots() {"Chưa có ảnh"} else {""}),
        "search" => TileView::card("TÌM KIẾM","Web / thư mục…"),
        "worldclock" => TileView::card("GIỜ THẾ GIỚI",""),
        "analog"|"flipclock"|"segclock" => TileView::card("",now.format("%H:%M").to_string()),
        "daynum" => TileView::card(now.format("%m / %Y").to_string(),now.day().to_string()),
        "sun" => { let value = d.weather.as_ref().and_then(|w| Some(format!("{} ↑  {} ↓",weather_time(w.sunrise?,w),weather_time(w.sunset?,w)))).unwrap_or("Chưa có dữ liệu".into()); TileView::card("MẶT TRỜI",value) }
        "hourly"|"forecast" => TileView::card(if id=="hourly" {"6 GIỜ TỚI"} else {"4 NGÀY TỚI"},if d.weather.is_some() {""} else {"Chưa có dữ liệu"}),
        "metric" => { let f=finance::snapshot(); TileView {sub:if f.btc_error {"Cũ"} else {"USD"}.into(),..TileView::card("BITCOIN",f.btc.map_or(if f.btc_error {"Lỗi mạng"} else {"Đang tải…"}.into(),|n|format!("${n:.0}")))} }
        "currency" => { let f=finance::snapshot(); TileView {style:Style::Metrics,
            sub:finance::convert(&f.rates,s.currency_amount,&s.currency_from,&s.currency_to).map_or("Chưa có tỷ giá".into(),|n|format!("{n:.2} {}{}",s.currency_to,if f.rates_error {" · cũ"} else {""})),
            ..TileView::card("ĐỔI TIỀN",format!("{} {}",s.currency_amount,s.currency_from))} }
        "screentime" => { let secs=crate::data::screentime::total_seconds();
            TileView::card("MÀN HÌNH HÔM NAY",duration_text(secs)) }
        _ => return None,
    };
    if matches!(id,"pomodoro"|"yearprog"|"worldclock"|"analog"|"flipclock"|"segclock"|"daynum"|"sun"|"hourly"|"forecast"|"recentshots") {
        v.style=Style::Widget;
    }
    v.extra=id.into(); Some(v)
}
fn duration_text(secs:f64) -> String { format!("{} giờ {} phút",secs as u64/3600,secs as u64/60%60) }
fn weather_time(at:i64,w:&crate::data::weather::Weather) -> String {
    chrono::DateTime::from_timestamp(at+w.utc_offset_seconds as i64,0).map_or("--:--".into(),|d|d.format("%H:%M").to_string())
}
fn weather_glyph(code:u32) -> &'static str { match code {0=>"☀",1..=3=>"☁",4..=48=>"≋",49..=67|78..=82=>"☂",68..=77|83..=86=>"❄",_=>"ϟ"} }

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
                        rows.push(super::dock::UsageRow {label:weather_time(h.at,w),value:format!("{:.0}°",h.temp),right:weather_glyph(h.code).into(),frac:0.0,color:BLUE});
                    }
                } else {
                    let local_date=chrono::DateTime::from_timestamp(Utc::now().timestamp()+w.utc_offset_seconds as i64,0).unwrap().date_naive();
                    for day in w.daily.iter().filter(|day|chrono::NaiveDate::parse_from_str(&day.date,"%Y-%m-%d").is_ok_and(|d|d>local_date)).take(4) {
                        rows.push(super::dock::UsageRow {label:day.date.get(5..).unwrap_or(&day.date).into(),value:format!("{:.0}/{:.0}°",day.min,day.max),right:weather_glyph(day.code).into(),frac:0.0,color:BLUE});
                    }
                }
                v.rows=rows;
                if v.rows.is_empty() { v.value="Chờ cập nhật".into(); }
            }
        }
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
    super::glass::draw_glass(r,rc,super::glass::GLASS_RADIUS,super::glass::Layer::Tile,hover)?;
    let now=Local::now();
    match t.extra.as_str() {
        "pomodoro" => {
            let a=rect(rc.x+7.0,rc.y+4.0,rc.w-38.0,11.0);
            text(r,&t.title,a,7.5,MUTED)?;
            center(r,&format!("{} {}",t.value,t.sub),rect(a.x,a.y+11.0,a.w,19.0),15.0,WHITE)?;
            let skip=rect(rc.x+rc.w-29.0,rc.y+8.0,23.0,26.0);
            r.fill_rounded_rect(skip,7.0,0xFFFF_FF20)?; center(r,"»",skip,19.0,WHITE)?;
            r.fill_rounded_rect(rect(a.x,rc.y+rc.h-4.0,(rc.w-14.0)*t.progress.unwrap_or(0.0),2.0),1.0,t.accent)?;
        }
        "analog" => {
            let (cx,cy)=(rc.x+rc.w/2.0,rc.y+rc.h/2.0); let radius=rc.w.min(rc.h)*0.40;
            r.stroke_arc(cx,cy,radius,0.0,360.0,0xFFFF_FF66,1.0)?;
            for i in 0..12 { let a=(i as f32*30.0-90.0).to_radians();
                r.stroke_line(cx+a.cos()*(radius-2.0),cy+a.sin()*(radius-2.0),cx+a.cos()*radius,cy+a.sin()*radius,WHITE,1.0)?; }
            for (angle,length,color,width) in [(now.hour() as f32*30.0+now.minute() as f32/2.0,radius*0.52,WHITE,2.0),
                (now.minute() as f32*6.0+now.second() as f32/10.0,radius*0.78,WHITE,1.5),(now.second() as f32*6.0,radius*0.82,ORANGE,1.0)] {
                let a=(angle-90.0).to_radians(); r.stroke_line(cx,cy,cx+a.cos()*length,cy+a.sin()*length,color,width)?;
            }
            r.fill_rounded_rect(rect(cx-2.0,cy-2.0,4.0,4.0),2.0,ORANGE)?;
        }
        "flipclock" => {
            let pitch=(rc.w-12.0)/4.0;
            for (i,ch) in t.value.chars().filter(|c|c.is_ascii_digit()).enumerate() {
                let cell=rect(rc.x+5.0+i as f32*pitch,rc.y+6.0,pitch-2.0,rc.h-12.0);
                r.fill_rounded_rect_gradient(cell,4.0,0x333B_49FF,0x171D_28FF)?;
                center(r,&ch.to_string(),cell,23.0,WHITE)?;
                r.stroke_line(cell.x,cell.y+cell.h/2.0,cell.x+cell.w,cell.y+cell.h/2.0,0x0000_0090,1.1)?;
            }
        }
        "segclock" => {
            let pitch=(rc.w-12.0)/5.0;
            for (i,ch) in t.value.chars().enumerate() {
                let a=rect(rc.x+6.0+i as f32*pitch,rc.y+9.0,pitch-4.0,rc.h-18.0);
                if ch==':' { for f in [0.33,0.66] {r.fill_rounded_rect(rect(a.x+a.w/2.0-1.0,a.y+a.h*f,2.5,2.5),1.0,GREEN)?;} }
                else if let Some(n)=ch.to_digit(10) { draw_digit(r,n as usize,a)?; }
            }
        }
        "daynum" => {
            let page=rect(rc.x+5.0,rc.y+3.0,rc.w-10.0,rc.h-6.0);
            r.fill_rounded_rect(page,5.0,0xF4F1_EAFF)?;
            r.fill_top_rounded_rect(rect(page.x,page.y,page.w,10.0),5.0,0xEB65_64FF)?;
            center(r,&format!("THÁNG {}",now.month()),rect(page.x,page.y,page.w,10.0),6.0,WHITE)?;
            center(r,&t.value,rect(page.x,page.y+10.0,page.w,page.h-10.0),22.0,0x2027_32FF)?;
        }
        "yearprog" => {
            let cols=53; let rows=7;
            let pitch=((rc.w-12.0)/cols as f32).min((rc.h-14.0)/rows as f32);
            text(r,&format!("{} · {}",now.year(),t.value),rect(rc.x+6.0,rc.y+1.0,rc.w-12.0,10.0),7.0,MUTED)?;
            for (i,&done) in t.cells.iter().enumerate() {
                let a=rect(rc.x+6.0+(i/7) as f32*pitch,rc.y+13.0+(i%7) as f32*pitch,(pitch-0.9).max(0.5),(pitch-0.9).max(0.5));
                r.fill_rounded_rect(a,0.7,if i==now.ordinal0() as usize {ORANGE} else if done>0 {GREEN} else {0xFFFF_FF30})?;
            }
        }
        "worldclock"|"hourly"|"forecast" => {
            let n=t.rows.len();
            if n==0 { text(r,&t.value,rect(rc.x+7.0,rc.y,rc.w-14.0,rc.h),10.0,MUTED)?; }
            else {
                let pitch=(rc.w-8.0)/n as f32;
                for (i,row) in t.rows.iter().enumerate() {
                    let a=rect(rc.x+4.0+i as f32*pitch,rc.y+3.0,pitch,11.0);
                    center(r,&row.label,a,7.5,MUTED)?;
                    center(r,&row.value,RectF{y:a.y+10.0,h:16.0,..a},if n>4 {9.0} else {11.0},WHITE)?;
                    center(r,&row.right,RectF{y:a.y+26.0,h:9.0,..a},8.0,if t.extra=="worldclock" {MUTED} else {ORANGE})?;
                }
            }
        }
        "recentshots" => {
            let shots=tools::shots();
            if shots.is_empty() {text(r,"Chưa có ảnh chụp",rect(rc.x+8.0,rc.y,rc.w-16.0,rc.h),10.0,MUTED)?;}
            let pitch=(rc.w-12.0)/3.0;
            for (i,shot) in shots.iter().enumerate() { draw_shot(r,shot,rect(rc.x+5.0+i as f32*pitch,rc.y+5.0,pitch-3.0,rc.h-10.0))?; }
        }
        "sun" => {
            let cx=rc.x+rc.w/2.0; let cy=rc.y+29.0; let rad=22.0;
            r.stroke_arc(cx,cy,rad,180.0,180.0,0xFFFF_FF50,1.5)?;
            if let Some(frac)=t.progress {let a=std::f32::consts::PI*(1.0-frac);r.fill_rounded_rect(rect(cx+a.cos()*rad-3.0,cy-a.sin()*rad-3.0,6.0,6.0),3.0,ORANGE)?;}
            text(r,&t.value,rect(rc.x+6.0,rc.y+30.0,rc.w-12.0,10.0),8.0,MUTED)?;
        }
        _=>{}
    }
    Ok(())
}
fn draw_digit(r:&mut Renderer,n:usize,a:RectF) -> Result<()> {
    const MASK:[u8;10]=[0x3f,0x06,0x5b,0x4f,0x66,0x6d,0x7d,0x07,0x7f,0x6f];
    let w=a.w; let h=a.h; let b=1.8;
    for (i,seg) in [rect(a.x+b,a.y,w-2.0*b,b),rect(a.x+w-b,a.y+b,b,h/2.0-b),
        rect(a.x+w-b,a.y+h/2.0,b,h/2.0-b),rect(a.x+b,a.y+h-b,w-2.0*b,b),
        rect(a.x,a.y+h/2.0,b,h/2.0-b),rect(a.x,a.y+b,b,h/2.0-b),rect(a.x+b,a.y+h/2.0-b/2.0,w-2.0*b,b)].iter().enumerate() {
        r.fill_rounded_rect(*seg,0.6,if MASK[n]&(1<<i)!=0 {GREEN} else {0x62DA_A01A})?;
    }
    Ok(())
}
fn draw_shot(r:&mut Renderer,shot:&tools::Shot,a:RectF) -> Result<()> {
    r.fill_rounded_rect(a,4.0,0x0000_0050)?;
    let scale=(a.w/shot.w as f32).min(a.h/shot.h as f32);
    let (w,h)=(shot.w as f32*scale,shot.h as f32*scale);
    r.draw_bitmap(shot.w,shot.h,&shot.rgba,rect(a.x+(a.w-w)/2.0,a.y+(a.h-h)/2.0,w,h))
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
                let pages=p::watch_summary().2.div_ceil(8).max(1);
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
