from pathlib import Path
p=Path(r'C:\Users\QUEEN\Projects\QDM\native\src\engine\transfer.rs')
s=p.read_text(encoding='utf8')
a='''        let mut buffer = vec![0; 1024 * 1024];
        let mut assembled = 0u64;'''
b='''        let mut buffer = vec![0; 1024 * 1024];
        // Controlled fixture pacing only; normal user transfers have no delay.
        let test_delay_ms = if std::env::var("QDM_TEST_MODE").ok().as_deref() == Some("1") {
            std::env::var("QDM_TEST_ASSEMBLY_DELAY_MS").ok()
                .and_then(|v| v.parse::<u64>().ok()).unwrap_or(0).min(30)
        } else { 0 };
        let mut assembled = 0u64;'''
assert s.count(a)==1, s.count(a)
s=s.replace(a,b)
a='''                assembled = assembled.saturating_add(count as u64);'''
b='''                assembled = assembled.saturating_add(count as u64);
                if test_delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(test_delay_ms)).await;
                }'''
assert s.count(a)==1,s.count(a)
s=s.replace(a,b)
p.write_text(s,encoding='utf8')
print('test-only controlled merge delay available')
