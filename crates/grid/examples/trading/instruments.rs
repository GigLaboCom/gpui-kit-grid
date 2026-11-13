//! The instruments the trading examples simulate: real US-listed tickers
//! and company names, their GICS sector and a seed price.
//!
//! **Nothing here is live market data.** The seed prices are approximate
//! levels typed in from memory, possibly far from today's; the simulator
//! (`market.rs`) random-walks from them. Every price, trade, order book,
//! position and headline the examples show is simulated.

/// A sector: the GICS sectors of the listed stocks, and index ETFs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sector {
    Technology,
    Financials,
    HealthCare,
    ConsumerDiscretionary,
    CommunicationServices,
    Industrials,
    ConsumerStaples,
    Energy,
    Utilities,
    Materials,
    IndexEtfs,
}

impl Sector {
    pub const ALL: [Self; 11] = [
        Self::Technology,
        Self::Financials,
        Self::HealthCare,
        Self::ConsumerDiscretionary,
        Self::CommunicationServices,
        Self::Industrials,
        Self::ConsumerStaples,
        Self::Energy,
        Self::Utilities,
        Self::Materials,
        Self::IndexEtfs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Technology => "Technology",
            Self::Financials => "Financials",
            Self::HealthCare => "Health Care",
            Self::ConsumerDiscretionary => "Consumer Discretionary",
            Self::CommunicationServices => "Communication Services",
            Self::Industrials => "Industrials",
            Self::ConsumerStaples => "Consumer Staples",
            Self::Energy => "Energy",
            Self::Utilities => "Utilities",
            Self::Materials => "Materials",
            Self::IndexEtfs => "Index ETFs",
        }
    }
}

/// One listed instrument: ticker, company or fund name, sector and the
/// approximate price the simulation starts from (not a live quote).
pub struct Instrument {
    pub ticker: &'static str,
    pub name: &'static str,
    pub sector: Sector,
    pub seed: f64,
}

const fn instrument(
    ticker: &'static str,
    name: &'static str,
    sector: Sector,
    seed: f64,
) -> Instrument {
    Instrument {
        ticker,
        name,
        sector,
        seed,
    }
}

use Sector::*;

/// The 150 instruments, by sector. Seed prices are approximate, from memory,
/// and only start the simulation: they are not market data.
pub static INSTRUMENTS: [Instrument; 150] = [
    // Technology (23)
    instrument("AAPL", "Apple", Technology, 230.0),
    instrument("MSFT", "Microsoft", Technology, 510.0),
    instrument("NVDA", "NVIDIA", Technology, 180.0),
    instrument("AVGO", "Broadcom", Technology, 340.0),
    instrument("ORCL", "Oracle", Technology, 250.0),
    instrument("CRM", "Salesforce", Technology, 250.0),
    instrument("ADBE", "Adobe", Technology, 350.0),
    instrument("AMD", "Advanced Micro Devices", Technology, 160.0),
    instrument("CSCO", "Cisco Systems", Technology, 68.0),
    instrument("ACN", "Accenture", Technology, 250.0),
    instrument("IBM", "IBM", Technology, 280.0),
    instrument("INTC", "Intel", Technology, 35.0),
    instrument("QCOM", "Qualcomm", Technology, 165.0),
    instrument("TXN", "Texas Instruments", Technology, 185.0),
    instrument("INTU", "Intuit", Technology, 650.0),
    instrument("NOW", "ServiceNow", Technology, 900.0),
    instrument("AMAT", "Applied Materials", Technology, 200.0),
    instrument("MU", "Micron Technology", Technology, 180.0),
    instrument("LRCX", "Lam Research", Technology, 140.0),
    instrument("ADI", "Analog Devices", Technology, 240.0),
    instrument("KLAC", "KLA", Technology, 900.0),
    instrument("PANW", "Palo Alto Networks", Technology, 200.0),
    instrument("SNPS", "Synopsys", Technology, 450.0),
    // Financials (18)
    instrument("JPM", "JPMorgan Chase", Financials, 300.0),
    instrument("BAC", "Bank of America", Financials, 50.0),
    instrument("WFC", "Wells Fargo", Financials, 82.0),
    instrument("GS", "Goldman Sachs", Financials, 780.0),
    instrument("MS", "Morgan Stanley", Financials, 155.0),
    instrument("C", "Citigroup", Financials, 100.0),
    instrument("BLK", "BlackRock", Financials, 1_150.0),
    instrument("SCHW", "Charles Schwab", Financials, 95.0),
    instrument("AXP", "American Express", Financials, 350.0),
    instrument("V", "Visa", Financials, 345.0),
    instrument("MA", "Mastercard", Financials, 570.0),
    instrument("SPGI", "S&P Global", Financials, 500.0),
    instrument("CB", "Chubb", Financials, 280.0),
    instrument("PGR", "Progressive", Financials, 240.0),
    instrument("BK", "Bank of New York Mellon", Financials, 105.0),
    instrument("USB", "U.S. Bancorp", Financials, 48.0),
    instrument("PNC", "PNC Financial Services", Financials, 190.0),
    instrument("BRK.B", "Berkshire Hathaway", Financials, 490.0),
    // Health Care (17)
    instrument("LLY", "Eli Lilly", HealthCare, 800.0),
    instrument("UNH", "UnitedHealth Group", HealthCare, 340.0),
    instrument("JNJ", "Johnson & Johnson", HealthCare, 190.0),
    instrument("ABBV", "AbbVie", HealthCare, 220.0),
    instrument("MRK", "Merck", HealthCare, 85.0),
    instrument("PFE", "Pfizer", HealthCare, 25.0),
    instrument("TMO", "Thermo Fisher Scientific", HealthCare, 480.0),
    instrument("ABT", "Abbott Laboratories", HealthCare, 130.0),
    instrument("DHR", "Danaher", HealthCare, 210.0),
    instrument("AMGN", "Amgen", HealthCare, 290.0),
    instrument("ISRG", "Intuitive Surgical", HealthCare, 470.0),
    instrument("BMY", "Bristol-Myers Squibb", HealthCare, 46.0),
    instrument("GILD", "Gilead Sciences", HealthCare, 115.0),
    instrument("VRTX", "Vertex Pharmaceuticals", HealthCare, 430.0),
    instrument("MDT", "Medtronic", HealthCare, 92.0),
    instrument("CVS", "CVS Health", HealthCare, 75.0),
    instrument("SYK", "Stryker", HealthCare, 380.0),
    // Consumer Discretionary (15)
    instrument("AMZN", "Amazon", ConsumerDiscretionary, 225.0),
    instrument("TSLA", "Tesla", ConsumerDiscretionary, 430.0),
    instrument("HD", "Home Depot", ConsumerDiscretionary, 390.0),
    instrument("MCD", "McDonald's", ConsumerDiscretionary, 300.0),
    instrument("NKE", "Nike", ConsumerDiscretionary, 70.0),
    instrument("LOW", "Lowe's", ConsumerDiscretionary, 250.0),
    instrument("SBUX", "Starbucks", ConsumerDiscretionary, 85.0),
    instrument("BKNG", "Booking Holdings", ConsumerDiscretionary, 5_300.0),
    instrument("TJX", "TJX Companies", ConsumerDiscretionary, 140.0),
    instrument("CMG", "Chipotle Mexican Grill", ConsumerDiscretionary, 40.0),
    instrument(
        "MAR",
        "Marriott International",
        ConsumerDiscretionary,
        270.0,
    ),
    instrument("GM", "General Motors", ConsumerDiscretionary, 65.0),
    instrument("F", "Ford Motor", ConsumerDiscretionary, 12.0),
    instrument("ORLY", "O'Reilly Automotive", ConsumerDiscretionary, 100.0),
    instrument("ROST", "Ross Stores", ConsumerDiscretionary, 150.0),
    // Communication Services (10)
    instrument("GOOGL", "Alphabet", CommunicationServices, 250.0),
    instrument("META", "Meta Platforms", CommunicationServices, 720.0),
    instrument("NFLX", "Netflix", CommunicationServices, 110.0),
    instrument("DIS", "Walt Disney", CommunicationServices, 112.0),
    instrument("CMCSA", "Comcast", CommunicationServices, 30.0),
    instrument("T", "AT&T", CommunicationServices, 27.0),
    instrument("VZ", "Verizon", CommunicationServices, 40.0),
    instrument("TMUS", "T-Mobile US", CommunicationServices, 230.0),
    instrument(
        "CHTR",
        "Charter Communications",
        CommunicationServices,
        260.0,
    ),
    instrument("EA", "Electronic Arts", CommunicationServices, 200.0),
    // Industrials (18)
    instrument("GE", "GE Aerospace", Industrials, 290.0),
    instrument("CAT", "Caterpillar", Industrials, 480.0),
    instrument("RTX", "RTX", Industrials, 160.0),
    instrument("HON", "Honeywell", Industrials, 210.0),
    instrument("UNP", "Union Pacific", Industrials, 225.0),
    instrument("BA", "Boeing", Industrials, 215.0),
    instrument("DE", "Deere", Industrials, 470.0),
    instrument("LMT", "Lockheed Martin", Industrials, 480.0),
    instrument("UPS", "United Parcel Service", Industrials, 85.0),
    instrument("ETN", "Eaton", Industrials, 360.0),
    instrument("ADP", "Automatic Data Processing", Industrials, 290.0),
    instrument("GD", "General Dynamics", Industrials, 330.0),
    instrument("NOC", "Northrop Grumman", Industrials, 590.0),
    instrument("WM", "Waste Management", Industrials, 215.0),
    instrument("CSX", "CSX", Industrials, 35.0),
    instrument("FDX", "FedEx", Industrials, 240.0),
    instrument("MMM", "3M", Industrials, 160.0),
    instrument("ITW", "Illinois Tool Works", Industrials, 250.0),
    // Consumer Staples (13)
    instrument("WMT", "Walmart", ConsumerStaples, 100.0),
    instrument("PG", "Procter & Gamble", ConsumerStaples, 150.0),
    instrument("COST", "Costco", ConsumerStaples, 920.0),
    instrument("KO", "Coca-Cola", ConsumerStaples, 70.0),
    instrument("PEP", "PepsiCo", ConsumerStaples, 145.0),
    instrument("PM", "Philip Morris International", ConsumerStaples, 155.0),
    instrument("MO", "Altria", ConsumerStaples, 60.0),
    instrument("MDLZ", "Mondelez International", ConsumerStaples, 60.0),
    instrument("CL", "Colgate-Palmolive", ConsumerStaples, 78.0),
    instrument("KMB", "Kimberly-Clark", ConsumerStaples, 120.0),
    instrument("GIS", "General Mills", ConsumerStaples, 48.0),
    instrument("TGT", "Target", ConsumerStaples, 95.0),
    instrument("KHC", "Kraft Heinz", ConsumerStaples, 26.0),
    // Energy (12)
    instrument("XOM", "Exxon Mobil", Energy, 115.0),
    instrument("CVX", "Chevron", Energy, 155.0),
    instrument("COP", "ConocoPhillips", Energy, 90.0),
    instrument("EOG", "EOG Resources", Energy, 110.0),
    instrument("SLB", "SLB", Energy, 35.0),
    instrument("PSX", "Phillips 66", Energy, 135.0),
    instrument("MPC", "Marathon Petroleum", Energy, 190.0),
    instrument("VLO", "Valero Energy", Energy, 165.0),
    instrument("OXY", "Occidental Petroleum", Energy, 43.0),
    instrument("WMB", "Williams Companies", Energy, 58.0),
    instrument("KMI", "Kinder Morgan", Energy, 27.0),
    instrument("HAL", "Halliburton", Energy, 24.0),
    // Utilities (10)
    instrument("NEE", "NextEra Energy", Utilities, 80.0),
    instrument("SO", "Southern Company", Utilities, 92.0),
    instrument("DUK", "Duke Energy", Utilities, 122.0),
    instrument("D", "Dominion Energy", Utilities, 60.0),
    instrument("AEP", "American Electric Power", Utilities, 115.0),
    instrument("EXC", "Exelon", Utilities, 45.0),
    instrument("SRE", "Sempra", Utilities, 90.0),
    instrument("XEL", "Xcel Energy", Utilities, 78.0),
    instrument("PCG", "PG&E", Utilities, 16.0),
    instrument("CEG", "Constellation Energy", Utilities, 350.0),
    // Materials (10)
    instrument("LIN", "Linde", Materials, 460.0),
    instrument("SHW", "Sherwin-Williams", Materials, 340.0),
    instrument("APD", "Air Products", Materials, 260.0),
    instrument("ECL", "Ecolab", Materials, 270.0),
    instrument("FCX", "Freeport-McMoRan", Materials, 42.0),
    instrument("NEM", "Newmont", Materials, 85.0),
    instrument("DOW", "Dow", Materials, 23.0),
    instrument("NUE", "Nucor", Materials, 140.0),
    instrument("DD", "DuPont", Materials, 35.0),
    instrument("PPG", "PPG Industries", Materials, 100.0),
    // Index ETFs (4)
    instrument("SPY", "SPDR S&P 500 ETF Trust", IndexEtfs, 670.0),
    instrument("QQQ", "Invesco QQQ Trust", IndexEtfs, 600.0),
    instrument("IWM", "iShares Russell 2000 ETF", IndexEtfs, 245.0),
    instrument(
        "DIA",
        "SPDR Dow Jones Industrial Average ETF",
        IndexEtfs,
        470.0,
    ),
];

/// The instrument with `ticker`, ignoring ASCII case.
pub fn find(ticker: &str) -> Option<InstrumentId> {
    INSTRUMENTS
        .iter()
        .position(|instrument| instrument.ticker.eq_ignore_ascii_case(ticker))
        .map(|ix| InstrumentId(ix as u16))
}

/// An instrument's place in [`INSTRUMENTS`]: the key of its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstrumentId(pub u16);

impl InstrumentId {
    pub fn get(self) -> &'static Instrument {
        &INSTRUMENTS[self.0 as usize]
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (0..INSTRUMENTS.len() as u16).map(Self)
    }
}

// ---------------------------------------------------------------------------
// Futures (the `ladder` example): a few index and commodity futures with
// their tick sizes and tick values, and the tick arithmetic of a price
// ladder. Seed prices are approximate levels typed in from memory, not
// market data; every price, trade and fill the examples show is simulated.
// ---------------------------------------------------------------------------

/// One futures contract: its symbol (root and month code), name, minimum
/// price step and what one step is worth, a typical resting size of one
/// book level, and the approximate price the simulation starts from.
pub struct Contract {
    /// Root and month code: `ESZ6` is `ES`, December 2026.
    pub symbol: &'static str,
    pub name: &'static str,
    /// The delivery month, `Dec 26`.
    pub month: &'static str,
    /// The minimum price step.
    pub tick: f64,
    /// What one tick is worth per contract, in US dollars.
    pub tick_value: f64,
    /// A typical resting size of one book level, in contracts.
    pub depth: i64,
    /// The approximate price the simulation starts from (not a quote).
    pub seed: f64,
}

const fn contract(
    symbol: &'static str,
    name: &'static str,
    month: &'static str,
    tick: f64,
    tick_value: f64,
    depth: i64,
    seed: f64,
) -> Contract {
    Contract {
        symbol,
        name,
        month,
        tick,
        tick_value,
        depth,
        seed,
    }
}

/// The futures, index first. Tick sizes and values are the contracts'
/// own; seeds are approximate and only start the simulation.
pub static CONTRACTS: [Contract; 8] = [
    contract("ESZ6", "E-mini S&P 500", "Dec 26", 0.25, 12.5, 120, 6_850.),
    contract("NQZ6", "E-mini Nasdaq-100", "Dec 26", 0.25, 5., 14, 25_200.),
    contract("YMZ6", "E-mini Dow ($5)", "Dec 26", 1., 5., 8, 47_000.),
    contract(
        "RTYZ6",
        "E-mini Russell 2000",
        "Dec 26",
        0.1,
        5.,
        12,
        2_480.,
    ),
    contract("CLF7", "Crude Oil (WTI)", "Jan 27", 0.01, 10., 18, 61.5),
    contract("NGF7", "Natural Gas", "Jan 27", 0.001, 10., 9, 3.85),
    contract("GCG7", "Gold", "Feb 27", 0.1, 10., 8, 4_050.),
    contract("SIH7", "Silver", "Mar 27", 0.005, 25., 5, 48.5),
];

/// A contract's place in [`CONTRACTS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContractId(pub u8);

impl ContractId {
    pub fn get(self) -> &'static Contract {
        &CONTRACTS[self.0 as usize]
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (0..CONTRACTS.len() as u8).map(Self)
    }

    /// The contract with `symbol` or root (`ESZ6`, `es`), ignoring case.
    pub fn find(symbol: &str) -> Option<Self> {
        Self::all().find(|id| {
            let own = id.get().symbol;
            own.eq_ignore_ascii_case(symbol) || own[..own.len() - 2].eq_ignore_ascii_case(symbol)
        })
    }
}

impl Contract {
    /// The tick `price` rounds to: prices are whole ticks, so a ladder's
    /// rows and its orders are keyed by tick, never by a float.
    pub fn ticks(&self, price: f64) -> i64 {
        (price / self.tick).round() as i64
    }

    /// The price of `ticks`.
    pub fn price(&self, ticks: i64) -> f64 {
        ticks as f64 * self.tick
    }

    /// The price of `ticks` with the tick's decimals: `6850.25`, `3.850`.
    pub fn format(&self, ticks: i64) -> String {
        format!("{:.*}", tick_decimals(self.tick), self.price(ticks))
    }

    /// What `ticks` ticks are worth on one contract, in US dollars.
    pub fn value(&self, ticks: f64) -> f64 {
        ticks * self.tick_value
    }
}

/// The decimals a price in steps of `tick` needs: 2 for 0.25 and 0.01, 1
/// for 0.1, 3 for 0.005, none for 1.
pub fn tick_decimals(tick: f64) -> usize {
    (0..=6)
        .find(|&decimals| {
            let scaled = tick * 10f64.powi(decimals as i32);
            (scaled - scaled.round()).abs() < 1e-6
        })
        .unwrap_or(6)
}

#[cfg(test)]
mod futures_tests {
    use super::*;

    #[test]
    fn test_prices_are_whole_ticks_with_the_ticks_decimals() {
        let es = ContractId::find("es").expect("ES").get();
        assert_eq!(es.ticks(6850.30), 27_401);
        assert_eq!(es.format(27_401), "6850.25");
        assert_eq!(es.value(4.), 50.);
        let ng = ContractId::find("NGF7").expect("NG").get();
        assert_eq!(ng.format(ng.ticks(3.85)), "3.850");
        assert_eq!(tick_decimals(0.1), 1);
        assert_eq!(tick_decimals(1.), 0);
        assert_eq!(tick_decimals(0.005), 3);
        assert!(ContractId::find("ZZ").is_none());
        // Every seed is a whole tick away from zero and positive.
        for id in ContractId::all() {
            let contract = id.get();
            assert!(contract.seed > 0. && contract.depth > 0);
            assert!((contract.price(contract.ticks(contract.seed)) - contract.seed).abs() < 1e-9);
        }
    }
}
